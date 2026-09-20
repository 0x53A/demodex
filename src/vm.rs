use anyhow::{Context, Result, ensure};
use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Create a private copy-on-write disk and SSH identity. Does not boot.
    Create {
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, default_value_t = 4096)]
        memory_mib: u32,
        #[arg(long, default_value_t = 2)]
        cpus: u16,
        #[arg(long, default_value_t = 2222)]
        ssh_port: u16,
        /// Isolated permits only forwarded SSH. NAT also permits outbound network access.
        #[arg(long, value_enum, default_value_t=Network::Isolated)]
        network: Network,
    },
    /// Run in the foreground. Ctrl-C stops this VM; disk changes persist.
    Run { directory: PathBuf },
    /// Show the stored VM definition.
    Inspect { directory: PathBuf },
}
#[derive(Serialize, Deserialize)]
struct Definition {
    base: PathBuf,
    memory_mib: u32,
    cpus: u16,
    ssh_port: u16,
    network: Network,
}

#[derive(Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Network {
    Isolated,
    Nat,
}

pub async fn execute(args: Args) -> Result<()> {
    match args.command {
        Action::Create {
            base,
            directory,
            memory_mib,
            cpus,
            ssh_port,
            network,
        } => {
            ensure!(
                memory_mib >= 512 && cpus > 0 && ssh_port > 0,
                "invalid VM resources"
            );
            let base = base.canonicalize().context("base image does not exist")?;
            // Fail if the directory already exists, preserving all existing VM state.
            std::fs::create_dir(&directory)
                .context("VM directory must be new and its parent must exist")?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            let disk = directory.join("disk.qcow2");
            success(
                Command::new("qemu-img")
                    .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
                    .arg(&base)
                    .arg(&disk),
            )
            .await?;
            success(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(directory.join("id_ed25519")),
            )
            .await?;
            let definition = Definition {
                base,
                memory_mib,
                cpus,
                ssh_port,
                network,
            };
            std::fs::write(
                directory.join("vm.json"),
                serde_json::to_vec_pretty(&definition)?,
            )?;
            println!(
                "Created {}\nRun: demodex vm run {}",
                directory.display(),
                directory.display()
            );
        }
        Action::Inspect { directory } => {
            println!("{}", serde_json::to_string_pretty(&read(&directory)?)?)
        }
        Action::Run { directory } => {
            let definition = read(&directory)?;
            let directory = directory.canonicalize()?;
            // Relative fixed filenames avoid QEMU option-string injection from host paths.
            let mut command = Command::new("qemu-system-x86_64");
            command.current_dir(&directory).kill_on_drop(true).args([
                "-machine",
                "q35,accel=kvm",
                "-cpu",
                "host",
                "-m",
                &definition.memory_mib.to_string(),
                "-smp",
                &definition.cpus.to_string(),
                "-display",
                "none",
                "-monitor",
                "none",
                "-serial",
                "stdio",
                "-drive",
                "file=disk.qcow2,format=qcow2,if=virtio",
                "-fw_cfg",
                "name=opt/demodex/authorized-key,file=id_ed25519.pub",
                "-netdev",
                &format!(
                    "user,id=net0,restrict={},hostfwd=tcp:127.0.0.1:{}-:22",
                    match definition.network {
                        Network::Isolated => "on",
                        Network::Nat => "off",
                    },
                    definition.ssh_port
                ),
                "-device",
                "virtio-net-pci,netdev=net0",
            ]);
            eprintln!(
                "SSH: ssh -i {}/id_ed25519 -p {} worker@127.0.0.1",
                directory.display(),
                definition.ssh_port
            );
            let mut child = command
                .spawn()
                .context("launch QEMU (requires /dev/kvm access)")?;
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                status=child.wait()=>{ensure!(status?.success(),"QEMU exited unsuccessfully");}
                signal=tokio::signal::ctrl_c()=>{signal?;let _=child.kill().await;}
                _=terminate.recv()=>{let _=child.kill().await;}
            }
        }
    }
    Ok(())
}
fn read(directory: &Path) -> Result<Definition> {
    Ok(serde_json::from_slice(&std::fs::read(
        directory.join("vm.json"),
    )?)?)
}
async fn success(command: &mut Command) -> Result<()> {
    let status = command.status().await?;
    ensure!(status.success(), "command failed: {command:?}");
    Ok(())
}
