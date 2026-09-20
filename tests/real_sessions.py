# /// script
# dependencies = []
# ///
"""Opt-in paid integration test: isolated Codex harness, real executor in a VM.

Requires an already authenticated, dedicated Demodex app-server and explicit
authorization for real turns. No credentials are read or copied.
Run with: uv run tests/real_sessions.py --run-real-sessions \
    --app-server unix:///absolute/data/runtime/ipc/app.sock IMAGE.qcow2
Approvals are never answered automatically. The runner prints pending requests;
use the normal manager API to review and answer them. Logs and guest disk remain
in .demodex/live-*/; the supplied app-server remains running.
"""
import argparse
import json
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-real-sessions', action='store_true', required=True)
    parser.add_argument('--app-server', required=True, help='Existing authenticated app-server endpoint')
    parser.add_argument('image', type=Path)
    args = parser.parse_args()
    run = ROOT / '.demodex' / ('live-' + uuid.uuid4().hex[:10])
    run.mkdir(mode=0o700, parents=True)
    print(f'RUN_DIRECTORY={run}', flush=True)
    processes, logs = [], []
    ssh_port, executor_port, manager_port = [free_port() for _ in range(3)]
    codex = Path(shutil.which('codex')).resolve()
    vm = run / 'vm'
    binary = ROOT / 'target/debug/demodex'
    ssh = ['ssh', '-F', '/dev/null', '-i', str(vm/'id_ed25519'), '-p', str(ssh_port),
           '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3',
           '-o', 'StrictHostKeyChecking=accept-new', '-o', f'UserKnownHostsFile={run}/known_hosts']

    def start(name, argv, **kwargs):
        log = (run / (name+'.log')).open('w')
        logs.append(log)
        process = subprocess.Popen(argv, stdout=log, stderr=log, **kwargs)
        processes.append(process)
        return process

    def remote(command, **kwargs):
        return subprocess.run(ssh+['worker@127.0.0.1', command], check=True, **kwargs)

    def ready():
        for _ in range(120):
            result = subprocess.run(ssh+['worker@127.0.0.1', 'true'], capture_output=True)
            if result.returncode == 0:
                return
            if vm_process.poll() is not None:
                raise RuntimeError('VM exited; inspect vm.log')
            time.sleep(1)
        raise RuntimeError('VM SSH did not become ready')

    def wait_port(port):
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.2):
                    return
            except OSError:
                time.sleep(.1)
        raise RuntimeError(f'port {port} did not start')

    try:
        subprocess.run([str(binary), 'vm', 'create', '--base', str(args.image.resolve()),
                        '--directory', str(vm), '--ssh-port', str(ssh_port),
                        '--memory-mib', '4096', '--network', 'nat'], check=True)
        vm_process = start('vm', [str(binary), 'vm', 'run', str(vm)], stdin=subprocess.DEVNULL)
        ready()
        print('VM_READY: copying only the Codex runtime closure into the private guest store', flush=True)
        closure = subprocess.check_output(['nix-store', '-qR', str(codex.parent.parent)], text=True).splitlines()
        export = subprocess.Popen(['nix-store', '--export', *closure], stdout=subprocess.PIPE)
        try:
            remote('sudo nix-store --import >/dev/null', stdin=export.stdout)
        finally:
            export.stdout.close()
            assert export.wait() == 0, 'Nix closure export failed'
        # Persistent guest service survives rebuild/reboot; it carries no model credentials.
        executor_module = '''{ lib, ... }: {
  systemd.services.demodex-executor = {
    wantedBy = [ "multi-user.target" ];
    after = [ "network.target" ];
    environment.HOME = "/home/worker";
    environment.PATH = lib.mkForce "/run/current-system/sw/bin";
    serviceConfig = {
      User = "worker";
      WorkingDirectory = "/workspace";
      ExecStart = "CODEX exec-server --listen ws://127.0.0.1:4501";
      Restart = "on-failure";
    };
  };
}
'''.replace('CODEX', str(codex))
        remote('sudo tee /etc/nixos/executor.nix >/dev/null', input=executor_module, text=True)
        # Start the initial executor without rebuilding on behalf of the model.
        remote(shlex.join(['sudo', 'systemd-run', '--unit=demodex-executor', '--uid=worker',
                          '--working-directory=/workspace', '--setenv=HOME=/home/worker',
                          '--setenv=PATH=/run/current-system/sw/bin', str(codex),
                          'exec-server', '--listen', 'ws://127.0.0.1:4501']))
        tunnel_args = ssh + ['-N', '-o', 'ExitOnForwardFailure=yes', '-L',
                            f'127.0.0.1:{executor_port}:127.0.0.1:4501', 'worker@127.0.0.1']
        tunnel = start('tunnel', tunnel_args)
        wait_port(executor_port)

        manager = start('manager', [str(binary), '--bind', f'127.0.0.1:{manager_port}',
                        '--data-dir', str(run/'manager'), '--web-dir', str(ROOT/'web/dist')])
        wait_port(manager_port)
        token = (run/'manager/access-token').read_text().strip()
        metadata = {'url':f'http://127.0.0.1:{manager_port}', 'executor_port':executor_port,
                    'app_server':args.app_server,'ssh_port':ssh_port, 'codex':str(codex)}
        (run/'connection.json').write_text(json.dumps(metadata, indent=2))

        def api(path, body=None):
            request = urllib.request.Request(metadata['url']+'/api'+path,
                data=None if body is None else json.dumps(body).encode(),
                headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
            try:
                with urllib.request.urlopen(request, timeout=65) as response:
                    return json.load(response)
            except urllib.error.HTTPError as error:
                raise RuntimeError(error.read().decode()) from error

        def session(name, task):
            entry = api('/sessions', {'name':name,'endpoint':args.app_server,
                'targets':[{'id':'work-'+uuid.uuid4().hex[:6], 'url':f'ws://127.0.0.1:{executor_port}', 'cwd':'/workspace'}]})
            path = '/sessions/'+entry['id']
            print(f'SESSION={entry["id"]} {name}', flush=True)
            api(path+'/connect', {})
            result = api(path+'/messages', {'text':task})
            turn_id = result['turn']['id']
            cursor, seen = 0, set()
            deadline = time.monotonic()+900
            while time.monotonic() < deadline:
                for event in api(path+'/events?after='+str(cursor)):
                    cursor = event['seq']
                    message = event['message']
                    method, params = message.get('method'), message.get('params', {})
                    if method == 'item/completed':
                        item = params['item']
                        if item['type'] == 'agentMessage':
                            print('AGENT:', item.get('text', ''), flush=True)
                        elif item['type'] == 'commandExecution':
                            print('COMMAND:', item.get('command'), 'EXIT:',item.get('exitCode'), flush=True)
                    if method == 'turn/completed' and params['turn']['id'] == turn_id:
                        (run/(entry['id']+'.json')).write_text(json.dumps(params['turn'],indent=2))
                        if params['turn']['status'] != 'completed':
                            raise RuntimeError('turn failed: '+json.dumps(params['turn']))
                        print('TURN_COMPLETE', name, flush=True)
                        return
                detail = api(path)
                for request in detail['pending']:
                    if request['state']=='pending' and request['key'] not in seen:
                        seen.add(request['key'])
                        print('REVIEW_REQUIRED:', json.dumps({'session':entry['id'],**request}), flush=True)
                if detail['session']['status']=='disconnected':
                    raise RuntimeError('app-server disconnected: '+str(detail['session']['error']))
                time.sleep(1)
            api(path+'/interrupt', {})
            raise RuntimeError('test turn exceeded 15 minutes; interrupted, not retried')

        common = ('This is an authorized, short Demodex integration test. Use only the attached work environment for tools. '
                  'It is a disposable NixOS VM with passwordless sudo; /workspace is guest-local. '
                  'Do not inspect credentials or access other hosts. Complete the task directly and report concrete checks. ')
        session('Workspace smoke', common+
            'Create /workspace/hello.py that prints exactly "demodex guest hello" and run it using an available interpreter '
            '(if Python is missing, use a short shell script instead). Also record hostname, id, and /etc/os-release. '
            'Do not rebuild or install packages yet. Keep this to a few tool calls.')
        session('Agent rebuilds NixOS', common+
            'Modify /etc/nixos/configuration.nix, preserving ./guest.nix and adding ./executor.nix to its imports. '
            'Add environment.etc."demodex-agent-proof".text = "rebuilt by the agent\\n"; '
            'add pkgs.hello to environment.systemPackages and define a oneshot systemd service demodex-proof '
            'wanted by multi-user.target which writes "service started" to /var/lib/demodex-proof. '
            'Run sudo nixos-rebuild switch inside this VM. Verify hello executes, /etc/demodex-agent-proof matches, '
            'and the service marker exists. Do not reboot; the test controller will reboot afterward. '
            'If rebuilding fails, diagnose and fix the guest configuration. Keep the final report short.')
        verification = 'test "$(cat /etc/demodex-agent-proof)" = "rebuilt by the agent" && hello && sudo test -f /var/lib/demodex-proof'
        remote(verification)
        old_boot = remote('cat /proc/sys/kernel/random/boot_id', capture_output=True).stdout
        subprocess.run(ssh+['worker@127.0.0.1','sudo reboot'], capture_output=True)
        time.sleep(5)
        ready()
        assert remote('cat /proc/sys/kernel/random/boot_id',capture_output=True).stdout != old_boot
        if tunnel.poll() is not None:
            tunnel = start('tunnel-reboot',tunnel_args)
            wait_port(executor_port)
        session('Verify rebuilt guest after reboot', common+
            'Read /etc/demodex-agent-proof, run hello, check systemctl status demodex-proof --no-pager '
            'and /var/lib/demodex-proof, and run the workspace script left by the first session. '
            'Report whether the rebuilt system and workspace survived reboot. Make no changes.')
        remote(verification)
        (run/'PASS').write_text('Three real sessions; agent rebuilt guest; independent checks and reboot passed.\n')
        print('PASS: three real sessions, agent-driven NixOS rebuild, reboot and independent verification', flush=True)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log in logs:
            log.close()
        print('Stopped test processes. Run data retained at',run,flush=True)


if __name__ == '__main__':
    main()
