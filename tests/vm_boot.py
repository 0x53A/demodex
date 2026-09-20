"""Boot a disposable NixOS image and verify guest root and persistent disk state.

uv run tests/vm_boot.py /nix/store/...-nixos-disk-image/nixos.qcow2
No host directories or Nix daemon are shared with the guest.
"""
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

ROOT=Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='demodex-vm-test-') as temporary:
    directory=Path(temporary)
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    vm=directory/'vm'
    subprocess.run([str(ROOT/'target/debug/demodex'),'vm','create','--base',sys.argv[1],'--directory',str(vm),'--ssh-port',str(port),'--memory-mib','2048'],check=True,capture_output=True)
    with (directory/'serial.log').open('w+') as log:
        process=subprocess.Popen([str(ROOT/'target/debug/demodex'),'vm','run',str(vm)],stdin=subprocess.DEVNULL,stdout=log,stderr=log)
        ssh=['ssh','-F','/dev/null','-i',str(vm/'id_ed25519'),'-p',str(port),'-o','BatchMode=yes','-o','ConnectTimeout=2','-o','StrictHostKeyChecking=accept-new','-o',f'UserKnownHostsFile={directory}/known_hosts','worker@127.0.0.1']
        def ready():
            for _ in range(90):
                result=subprocess.run(ssh+['true'],capture_output=True)
                if result.returncode==0:return
                if process.poll() is not None:break
                time.sleep(1)
            log.flush();print((directory/'serial.log').read_text()[-10000:])
            raise AssertionError('guest SSH did not become ready')
        try:
            ready()
            subprocess.run(ssh+['sudo -n true && test -d /workspace && sudo test -w /etc/nixos/configuration.nix && printf persistent > /workspace/probe'],check=True)
            subprocess.run(ssh+['sudo nix-instantiate "<nixpkgs/nixos>" -A system --no-gc-warning >/dev/null'],check=True)
            old_boot=subprocess.check_output(ssh+['cat /proc/sys/kernel/random/boot_id'])
            reboot=subprocess.run(ssh+['sudo reboot'],capture_output=True)
            assert reboot.returncode in (0,255), reboot.stderr.decode()
            time.sleep(5)
            ready()
            assert subprocess.check_output(ssh+['cat /proc/sys/kernel/random/boot_id'])!=old_boot
            result=subprocess.check_output(ssh+['cat /workspace/probe']).decode()
            assert result=='persistent'
            print('PASS: NixOS boot, injected public key, guest sudo, writable configuration evaluates, workspace survives reboot')
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:process.wait(timeout=10)
                except subprocess.TimeoutExpired:process.kill();process.wait()
