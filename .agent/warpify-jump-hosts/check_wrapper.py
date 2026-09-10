import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parents[2]
source = (root / 'app/assets/bundled/bootstrap/zsh_body.sh').read_text()
helper = source[source.index('      function warp_ssh_helper()'):source.index('      function ssh()')]
setup = r'''
function command() {
  if [[ "$1" == ssh ]]; then
    if [[ "$2" == -G ]]; then
      printf 'remotecommand none\ncontrolpath none\n'
    else
      printf '%s' "${argv[-1]}"
    fi
  else
    builtin command "$@"
  fi
}
WARP_SESSION_ID=123
SSH_SOCKET_DIR=/tmp
WARP_SSH_REUSE_CONTROL_MASTER=0
OSC_START=$'\e]9278;'
DCS_JSON_MARKER=d
OSC_PARAM_SEPARATOR=';'
OSC_END=$'\a'
'''
generated = subprocess.run(['/bin/zsh', '-f', '-c', setup + helper + '\nwarp_ssh_helper example'], capture_output=True, check=True).stdout
result = subprocess.run(['/bin/bash', '-n'], input=generated, capture_output=True)
print('Generated remote script syntax:', result.returncode, result.stderr.decode())
if __name__ == '__main__':
    import re
    import json
    prefix = generated.split(b'\nif test ')[0]
    remote = subprocess.run(['ssh', '-A', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=15', '-tt', 'tx-jump', prefix.decode()], stdin=subprocess.DEVNULL, capture_output=True, timeout=20)
    for payload in re.findall(rb'\x1b\]9278;d;([^\x07]*)\x07', remote.stdout):
        print('Remote payload length:', len(payload))
        if payload:
            hook = json.loads(bytes.fromhex(payload.decode()))
            print('Hook:', hook['hook'], 'remote shell:', hook['value']['remote_shell'])
    print('Remote stderr:', remote.stderr.decode())
