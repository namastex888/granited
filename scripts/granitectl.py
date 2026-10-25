#!/usr/bin/env python3
"""Install granited on this machine, or add another machine over SSH, without manual steps.

  granitectl scan [--ssh HOST]          inventory and the deployment plan, changes nothing
  granitectl install [--yes]            plan, fetch pinned runtimes, configure, run as a user service
  granitectl node add HOST [--yes]      ship this checkout to HOST, install it there, link and enroll it
  granitectl node list | node remove NAME
  granitectl service status|start|stop|restart|logs
  granitectl uninstall                  stop and remove the services; files stay

Every download is pinned: llama.cpp archives and model files are verified against SHA-256 values
recorded in this repository before use. Nothing is adopted: engines already running on a machine are
reported, never taken over. Stdlib only; Python 3.9+ (the probe it ships runs on 3.8+).
"""
import argparse
import hashlib
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import tarfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
STATE = ROOT / 'state'
RECORD = STATE / 'install.json'
KEYS = STATE / 'secrets/keys.json'
GIB = 1024 ** 3
# Budgets mirror configure.py; reserve is what the OS and other work keep.
LANGUAGE_BUDGET = {'8b': 9 * GIB, '3b': 5 * GIB}
NATIVE_BUDGET = {'embedding': 2 * GIB, 'timeseries': 2 * GIB, 'speech': 4 * GIB}
RESERVE = 8 * GIB


def reserve_for(total):
    """Keep 8 GiB for the OS on large machines and a quarter of RAM (at least 1 GiB) on small ones."""
    return int(min(RESERVE, max(GIB, total / 4)))


def say(message):
    print(f'granitectl: {message}', flush=True)


def sh(*cmd, check=True, capture=False, env=None, cwd=None):
    result = subprocess.run([str(c) for c in cmd], check=False, text=True, env=env, cwd=cwd,
                            stdout=subprocess.PIPE if capture else None, stderr=subprocess.STDOUT if capture else None)
    if check and result.returncode != 0:
        tail = (result.stdout or '').strip().splitlines()[-5:] if capture else []
        raise SystemExit(f'command failed ({result.returncode}): {" ".join(map(str, cmd))}\n' + '\n'.join(tail))
    return result.stdout if capture else result.returncode


def load_json(path, default=None):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return default


def write_json(path, value, private=False):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + '\n')
    if private:
        path.chmod(0o600)


# ---------------------------------------------------------------- inventory and planning

def probe_local():
    return json.loads(sh(sys.executable, ROOT / 'scripts/probe.py', capture=True))


def ssh_cmd(target, jump=None):
    return ['ssh', '-o', 'BatchMode=yes', *(['-J', jump] if jump else []), target]


def probe_ssh(target, jump=None):
    with open(ROOT / 'scripts/probe.py', 'rb') as script:
        out = subprocess.run([*ssh_cmd(target, jump), 'python3 -'], stdin=script,
                             capture_output=True, text=True, timeout=180)
    if out.returncode != 0:
        raise SystemExit(f'probe over ssh failed on {target}: {out.stderr.strip() or out.stdout.strip()}')
    return json.loads(out.stdout)


def plan(inv, languages=None, native=None, mac_engine='auto'):
    """Map measured hardware to Granite deployments. Every skipped device says why."""
    system, machine = inv['system'], inv['machine'].lower()
    arch = 'arm64' if machine in ('arm64', 'aarch64') else 'x86_64' if machine in ('x86_64', 'amd64') else machine
    mem = inv['memory']
    total = mem.get('total_bytes') or 0
    reserve = reserve_for(total)
    notes, skipped = [], []
    gpus = [g for g in inv['gpus'] if g['kind'] != 'vulkan-summary']
    nvidia = [g for g in gpus if g['kind'] == 'nvidia']
    igpu = [g for g in gpus if g['kind'] in ('amd', 'intel') and (g.get('vulkan_icd') or g.get('vulkan'))]
    llama = None
    if system == 'Darwin' and arch == 'arm64':
        omlx = inv['engines']['installed'].get('omlx')
        engine = 'omlx' if mac_engine == 'omlx' or (mac_engine == 'auto' and omlx) else 'llama.cpp'
        if engine == 'omlx' and not omlx:
            raise SystemExit('--mac-engine omlx needs oMLX installed (https://github.com/jundot/omlx)')
        llama = dict(device='metal', asset='macos-arm64', pool_bytes=total, pool='unified memory', engine=engine, omlx=omlx)
    elif system == 'Linux' and nvidia and arch == 'x86_64':
        llama = dict(device='cuda', asset='linux-x86_64-cuda', pool_bytes=nvidia[0]['memory_bytes'], pool=f'{nvidia[0]["name"]} VRAM')
    elif system == 'Linux' and igpu:
        llama = dict(device='vulkan', asset=f'linux-{"aarch64" if arch == "arm64" else "x86_64"}-vulkan',
                     pool_bytes=total, pool=f'{igpu[0]["kind"].upper()} iGPU over shared RAM')
    elif system == 'Linux':
        llama = dict(device='cpu', asset=f'linux-{"aarch64" if arch == "arm64" else "x86_64"}-cpu', pool_bytes=total, pool='CPU')
    for g in gpus:
        if g['kind'] in ('amd', 'intel') and g not in igpu:
            skipped.append(f'{g["kind"]} GPU {g.get("node", "")}: no Vulkan driver (install mesa-vulkan-drivers)')
    for npu in inv.get('npus', []):
        installed = inv['engines']['installed']
        lemonade = next((e for e in inv['engines']['running'] if e['engine'] == 'lemonade'), None)
        granite = [m for m in (lemonade or {}).get('models', []) if 'granite' in m.lower()]
        if npu.get('kind') == 'amd-xdna' and granite:
            notes.append(f'NPU {npu.get("device")}: Lemonade serves {", ".join(granite)}; attach it with scripts/attach_llama.py-style registration after review')
        elif npu.get('kind') == 'amd-xdna':
            owner = ' (Lemonade installed by another service; left untouched)' if any('lemonade' in k for k in installed) else ''
            skipped.append(f'NPU {npu.get("device") or npu["node"]}: XDNA present, XRT {npu.get("xrt")}, but no Granite build for FastFlowLM/Lemonade is available{owner}')
        else:
            skipped.append(f'NPU {npu["node"]} ({npu.get("driver")}): no validated Granite runtime')
    # The pool is where weights live; RAM must also hold the reserve, native workers and the OS.
    sizes = []
    if llama:
        pool = llama['pool_bytes'] - (0 if llama['device'] == 'cuda' else reserve)
        wanted = languages if languages is not None else ['8b', '3b']
        for size in wanted:
            if LANGUAGE_BUDGET[size] <= pool and (llama['device'] == 'cuda' or LANGUAGE_BUDGET[size] + reserve <= total):
                sizes.append(size)
        if languages is None and sizes:
            sizes = sizes[:1] if pool < LANGUAGE_BUDGET['8b'] + LANGUAGE_BUDGET['3b'] else sizes
        if not sizes:
            if languages == []:
                skipped.append('language: not requested')
            else:
                skipped.append(f'language: {llama["pool"]} has {pool / GIB:.1f} GiB after reserve; Granite 4.2 3B needs 5 GiB')
            llama = None
        elif llama['device'] == 'cpu':
            notes.append('language runs on CPU only; expect a few tokens per second')
    lock = {('Darwin', 'arm64'): 'locks/macos-arm64-native.txt', ('Linux', 'x86_64'): 'locks/linux-x86_64-cpu.txt'}.get((system, arch))
    families = native if native is not None else ['embedding', 'timeseries', 'speech']
    natives = []
    if lock:
        budget = reserve + sum(LANGUAGE_BUDGET[s] for s in sizes[:1])
        for family in families:
            if budget + NATIVE_BUDGET[family] <= total:
                natives.append(family)
                budget += NATIVE_BUDGET[family]
            else:
                skipped.append(f'{family}: not enough RAM beside the language pool')
    elif families:
        skipped.append(f'native workers: no hashed Python lock for {system} {arch}')
    if mem.get('memory_stall_percent'):
        notes.append(f'memory is stalling {mem["memory_stall_percent"]}% now; cold loads pause above 5%')
    if mem.get('swap_total_bytes') and mem.get('swap_used_bytes', 0) > 0.9 * mem['swap_total_bytes']:
        notes.append('swap is over 90% used; admission uses available RAM and memory stall, not swap, but watch it')
    if llama and llama.get('engine') == 'omlx':
        notes.append('Granite 4.2 runs on oMLX with the official ibm-granite MLX builds (about 2x llama.cpp decode here); '
                     'pass --mac-engine llama for agents that resend one long prompt, where llama.cpp caches better')
    for engine in inv['engines']['running']:
        if engine['engine'] != 'granited':
            notes.append(f'{engine["engine"]} already serves on :{engine["port"]}; it is left alone')
    host = 'mac' if system == 'Darwin' else 'cuda' if nvidia else 'cpu'
    return dict(host=host, arch=arch, reserve_gib=round(reserve / GIB, 2), llama=llama, languages=sizes, native=natives, native_lock=lock,
                notes=notes, skipped=skipped, existing_granited=any(e['engine'] == 'granited' for e in inv['engines']['running']))


def show(inv, p):
    mem = inv['memory']
    print(f'\n{inv["hostname"]}: {inv["distro"]} {inv["machine"]}, {inv["cpu"]}, {inv["logical_cpus"]} threads, '
          f'{mem["total_bytes"] / GIB:.0f} GiB RAM ({mem["available_bytes"] / GIB:.0f} GiB available)')
    for g in inv['gpus']:
        if g['kind'] != 'vulkan-summary':
            print(f'  device  {g["kind"]} {g.get("name") or g.get("node")}' + (f', {g["memory_bytes"] / GIB:.0f} GiB' if g.get('memory_bytes') else ''))
    for n in inv.get('npus', []):
        print(f'  device  NPU {n.get("device") or n["node"]} ({n.get("driver")})')
    print('plan:')
    if p['llama']:
        engine = 'oMLX' if p['llama'].get('engine') == 'omlx' else 'llama.cpp b11669'
        print(f'  + {engine} on {p["llama"]["device"]} ({p["llama"]["pool"]}): Granite 4.2 ' + ', '.join(s.upper() for s in p['languages']))
    for family in p['native']:
        print(f'  + {family} worker on CPU')
    for line in p['skipped']:
        print(f'  - {line}')
    for line in p['notes']:
        print(f'  ! {line}')
    if not p['llama'] and not p['native']:
        print('  nothing deployable on this machine')


# ---------------------------------------------------------------- installation steps

def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(url, dest, sha256):
    dest = Path(dest)
    if dest.exists() and sha256_file(dest) == sha256:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_suffix(dest.suffix + '.part')
    say(f'downloading {url.rsplit("/", 1)[-1]}')
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as r, open(part, 'wb') as out:
        while chunk := r.read(1 << 20):
            digest.update(chunk)
            out.write(chunk)
    if digest.hexdigest() != sha256:
        part.unlink()
        raise SystemExit(f'{url} does not match its pinned SHA-256; refusing to use it')
    part.rename(dest)
    return dest


def ensure_uv():
    uv = shutil.which('uv')
    if uv:
        return uv
    local = ROOT / '.tools/uv/uv'
    if not local.exists():
        machine = 'arm64' if platform.machine().lower() in ('arm64', 'aarch64') else 'x86_64'
        asset = load_json(ROOT / 'locks/uv-releases.json')['assets'][f'{platform.system()}-{machine}']
        archive = fetch(asset['url'], STATE / 'downloads' / asset['name'], asset['sha256'])
        with tarfile.open(archive) as tar:
            member = next(m for m in tar.getmembers() if m.name.endswith('/uv'))
            local.parent.mkdir(parents=True, exist_ok=True)
            local.write_bytes(tar.extractfile(member).read())
        local.chmod(0o755)
    return str(local)


def ensure_binary():
    binary = ROOT / 'bin/granited'
    built = ROOT / 'target/release/granited'
    cargo = shutil.which('cargo') or str(Path.home() / '.cargo/bin/cargo')
    if not Path(cargo).exists():
        say('installing the Rust toolchain with rustup (toolchain pinned by rust-toolchain.toml)')
        script = subprocess.run(['curl', '--proto', '=https', '--tlsv1.2', '-sSf', 'https://sh.rustup.rs'],
                                capture_output=True, check=True).stdout
        subprocess.run(['sh', '-s', '--', '-y', '--profile', 'minimal', '--default-toolchain', 'none', '--no-modify-path'],
                       input=script, check=True)
        cargo = str(Path.home() / '.cargo/bin/cargo')
    if not (shutil.which('cc') or shutil.which('gcc') or shutil.which('clang')):
        if shutil.which('apt-get') and sh('sudo', '-n', 'true', check=False, capture=True) is not None \
                and subprocess.run(['sudo', '-n', 'true'], capture_output=True).returncode == 0:
            say('installing a C linker (build-essential) with passwordless sudo')
            sh('sudo', '-n', 'apt-get', 'install', '-y', '-q', 'build-essential')
        else:
            raise SystemExit('building granited needs a C linker: install build-essential (Debian/Ubuntu), gcc, '
                             'or the Xcode command line tools, then run this again')
    if not built.exists() or any(p.stat().st_mtime > built.stat().st_mtime for p in (ROOT / 'src').rglob('*')):
        say('building granited (cargo build --release --locked)')
        env = dict(os.environ, PATH=f'{Path(cargo).parent}:{os.environ["PATH"]}')
        sh(cargo, 'build', '--release', '--locked', cwd=ROOT, env=env)
    binary.parent.mkdir(exist_ok=True)
    if not binary.exists() or binary.read_bytes() != built.read_bytes():
        tmp = binary.with_suffix('.new')
        shutil.copy2(built, tmp)
        tmp.rename(binary)
    return binary


def install_llama(asset_key):
    lock = load_json(ROOT / 'locks/llama-cpp-releases.json')
    asset = lock['assets'][asset_key]
    archive = fetch(asset['url'], STATE / 'downloads' / asset['name'], asset['sha256'])
    target = STATE / 'runtime' / asset['name'][:-len('.tar.gz')]
    if not target.exists():
        tmp = target.with_suffix('.extract')
        shutil.rmtree(tmp, ignore_errors=True)
        with tarfile.open(archive) as tar:
            tar.extractall(tmp, filter='data') if sys.version_info >= (3, 12) else tar.extractall(tmp)
        tmp.rename(target)
    server = next(target.rglob('llama-server'), None)
    if not server:
        raise SystemExit(f'{asset["name"]} has no llama-server')
    return server


def install_models(sizes, models_dir):
    for size in sizes:
        info = load_json(ROOT / f'reports/gguf-{size}-files.json')
        name = f'granite-4.2-{size}-Q4_K_M.gguf'
        file = next(f for f in info['siblings'] if f['rfilename'] == name)
        fetch(f'https://huggingface.co/{info["id"]}/resolve/{info["sha"]}/{name}', Path(models_dir) / name, file['lfs']['sha256'])


def install_mlx(sizes, models_dir):
    """Official ibm-granite MLX builds; LFS files by SHA-256, small files by their Git blob hash."""
    for size in sizes:
        info = load_json(ROOT / f'reports/mlx-{size}-files.json')
        target = Path(models_dir) / 'mlx' / info['id'].split('/')[1]
        for f in info['files']:
            url = f'https://huggingface.co/{info["id"]}/resolve/{info["sha"]}/{f["path"]}'
            if 'sha256' in f:
                fetch(url, target / f['path'], f['sha256'])
                continue
            dest = target / f['path']
            data = dest.read_bytes() if dest.exists() else urllib.request.urlopen(url, timeout=60).read()
            if hashlib.sha1(b'blob %d\0' % len(data) + data).hexdigest() != f['git_blob_sha1']:
                raise SystemExit(f'{url} does not match its pinned Git blob hash; refusing to use it')
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)


def install_native(uv, lock):
    venv = ROOT / '.venv-native'
    if not (venv / 'bin/python').exists():
        sh(uv, 'venv', '--python', '3.12.12', venv)
    say(f'syncing native workers from {lock} (hash-checked)')
    extra = ['--torch-backend', 'cpu'] if 'linux' in lock else []
    sh(uv, 'pip', 'sync', '--python', venv / 'bin/python', '--require-hashes', *extra, ROOT / lock)


def service_paths(instance):
    if platform.system() == 'Darwin':
        label = f'ai.granited.{instance}'
        return label, Path.home() / f'Library/LaunchAgents/{label}.plist'
    unit = f'granited-{instance}'
    return unit, Path.home() / f'.config/systemd/user/{unit}.service'


def install_service(instance, program, description, keep_alive=True):
    name, path = service_paths(instance)
    # Another checkout (or a hand-written unit) may already own this name; never replace it.
    if path.exists() and str(ROOT) not in path.read_text():
        raise SystemExit(f'{path} belongs to another installation; pass --instance NAME (or --remote-instance) '
                         f'to install beside it')
    path.parent.mkdir(parents=True, exist_ok=True)
    log = STATE / f'{instance}.log'
    if platform.system() == 'Darwin':
        args = ''.join(f'<string>{a}</string>' for a in program)
        path.write_text(f'''<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{name}</string>
<key>ProgramArguments</key><array>{args}</array>
<key>WorkingDirectory</key><string>{ROOT}</string>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key>{"<dict><key>SuccessfulExit</key><false/></dict>" if keep_alive else "<false/>"}
<key>ThrottleInterval</key><integer>5</integer>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string>
<key>Umask</key><integer>63</integer>
</dict></plist>
''')
        domain = f'gui/{os.getuid()}'
        sh('launchctl', 'bootout', f'{domain}/{name}', check=False, capture=True)
        sh('launchctl', 'bootstrap', domain, path)
    else:
        path.write_text(f'''[Unit]
Description={description}
After=network-online.target

[Service]
Type=simple
WorkingDirectory={ROOT}
ExecStart={" ".join(shlex.quote(str(a)) for a in program)}
Restart={"on-failure" if keep_alive else "no"}
RestartSec=3
KillMode=mixed
TimeoutStopSec=650
UMask=0077

[Install]
WantedBy=default.target
''')
        sh('systemctl', '--user', 'daemon-reload')
        sh('systemctl', '--user', 'enable', '--now', name)
        if sh('loginctl', 'enable-linger', os.environ.get('USER', ''), check=False, capture=True) is None:
            pass
    return name


def service_action(instance, action):
    name, path = service_paths(instance)
    if platform.system() == 'Darwin':
        domain = f'gui/{os.getuid()}'
        commands = {'start': ['bootstrap', domain, str(path)], 'stop': ['bootout', f'{domain}/{name}'],
                    'restart': ['kickstart', '-k', f'{domain}/{name}'], 'status': ['print', f'{domain}/{name}']}
        return sh('launchctl', *commands[action], check=action != 'status')
    return sh('systemctl', '--user', action, name, check=action != 'status')


def port_open(port):
    import socket
    with socket.socket() as s:
        s.settimeout(0.3)
        return s.connect_ex(('127.0.0.1', port)) == 0


def wait_healthy(port, seconds=60):
    for _ in range(seconds * 4):
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz', timeout=1):
                return True
        except OSError:
            time.sleep(0.25)
    return False


def admin(port, path, body=None, key=None):
    key = key or load_json(KEYS)['GRANITE_ADMIN_KEY']
    req = urllib.request.Request(f'http://127.0.0.1:{port}{path}', data=json.dumps(body).encode() if body is not None else None,
                                 headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def install(args):
    inv = probe_local()
    languages = None if args.languages == 'auto' else [s for s in args.languages.split(',') if s and s != 'none']
    native = None if args.native == 'auto' else [s for s in args.native.split(',') if s and s != 'none']
    p = plan(inv, languages, native, args.mac_engine)
    show(inv, p)
    record = load_json(RECORD, {})
    if p['existing_granited'] and args.port == 8310 and record.get('port') != 8310:
        raise SystemExit('granited already answers on :8310 here; pass --port to install a second instance beside it')
    if not p['llama'] and not p['native']:
        raise SystemExit('nothing to install on this machine')
    if not args.yes and input('\ninstall this plan? [y/N] ').strip().lower() != 'y':
        raise SystemExit('cancelled')
    language_ports = [15 + i for i in range(len(p['languages']))] if (p['llama'] or {}).get('engine') == 'omlx' else [11]
    offsets = (language_ports if p['llama'] else []) + [{'embedding': 12, 'timeseries': 13, 'speech': 14}[f] for f in p['native']]
    taken = [args.port + o for o in offsets if port_open(args.port + o)]
    if taken:
        raise SystemExit(f'worker ports {taken} are already in use by other software; choose another --port '
                         f'(workers use port+11..port+14)')
    started = time.monotonic()
    STATE.mkdir(exist_ok=True)
    uv = ensure_uv()
    binary = ensure_binary()
    configure = [sys.executable, ROOT / 'scripts/configure.py', '--host', p['host'], '--profile', args.instance,
                 '--host-id', args.host_id or inv['hostname'].split('.')[0].lower(), '--port', args.port,
                 '--languages', ','.join(p['languages']) or 'none', '--native-models', ','.join(p['native']) or 'none',
                 '--models-dir', args.models_dir, '--ram-reserve-gib', p['reserve_gib']]
    if p['llama'] and p['llama'].get('engine') == 'omlx':
        install_mlx(p['languages'], args.models_dir)
        configure += ['--omlx', p['llama']['omlx']]
    elif p['llama']:
        server = install_llama(p['llama']['asset'])
        install_models(p['languages'], args.models_dir)
        configure += ['--llama-runtime', server, '--llama-device', p['llama']['device']]
    if p['native']:
        install_native(uv, p['native_lock'])
        configure.append('--native')
    say('writing the node profile')
    sh(*configure)
    # Run the service under the interpreter that will exist after this installer exits.
    python = ROOT / '.venv-native/bin/python' if (ROOT / '.venv-native/bin/python').exists() else Path(sys.executable)
    program = [python, ROOT / 'scripts/run_service.py', 'router', '--config', f'configs/{args.instance}.json']
    if args.no_service:
        # A plain detached process for containers, CI and trial runs; nothing persists across reboot.
        with open(STATE / f'{args.instance}.log', 'ab') as log:
            child = subprocess.Popen([str(a) for a in program], cwd=ROOT, stdout=log, stderr=log, start_new_session=True)
        write_json(STATE / f'{args.instance}.process.json', dict(pid=child.pid, command=[str(a) for a in program]))
        name = f'pid {child.pid}'
    else:
        name = install_service(args.instance, program, f'granited {args.instance}')
    write_json(RECORD, dict(instance=args.instance, port=args.port, service=None if args.no_service else name,
                            profile=f'configs/{args.instance}.json', installed_unix=int(time.time()), binary=str(binary)))
    if not wait_healthy(args.port):
        raise SystemExit(f'{name} did not become healthy; see state/{args.instance}.log')
    if platform.system() == 'Darwin' and not args.no_service:
        write_app_config(args.instance, args.port, name)
    for family, alias in (('embedding', 'granite-embedding-97m-multilingual-r2'), ('timeseries', 'granite-timeseries-ttm-r3'),
                          ('speech', 'granite-speech-5.0-470m-turboctc')):
        if family in p['native']:
            op = admin(args.port, f'/admin/models/{alias}/prefetch', {})
            say(f'prefetching {alias} (operation {op["id"]})')
    minutes = (time.monotonic() - started) / 60
    say(f'installed in {minutes:.1f} min: OpenAI endpoint http://127.0.0.1:{args.port}/v1 (keys in {KEYS})'
        + ('; build the menu-bar app with apps/macos/build-app.sh' if platform.system() == 'Darwin' else ''))


# ---------------------------------------------------------------- nodes over SSH

def ship(target, remote_dir, jump=None):
    say(f'copying this checkout to {target}:{remote_dir}')
    if (ROOT / '.git').exists():
        archive = subprocess.run(['git', '-C', ROOT, 'archive', '--format=tar', 'HEAD'], capture_output=True, check=True).stdout
    else:
        listing = subprocess.run(['tar', '-cf', '-', '--exclude=./state', '--exclude=./target', '--exclude=./.venv*',
                                  '--exclude=./models', '--exclude=./bin', '-C', ROOT, '.'], capture_output=True, check=True)
        archive = listing.stdout
    q = shlex.quote(remote_dir)
    subprocess.run([*ssh_cmd(target, jump), f'mkdir -p {q} && tar -xf - -C {q}'], input=archive, check=True)


def free_port(start=8340):
    peers = load_json(ROOT / load_json(RECORD)['profile'])['peers']
    used = {int(p['url'].rsplit(':', 1)[1]) for p in peers if p['url'].rsplit(':', 1)[1].isdigit()}
    port = start
    while port in used or port_open(port):
        port += 1
    return port


def node_add(args):
    record = load_json(RECORD)
    if not record:
        raise SystemExit('install granited on this machine first (granitectl install); it becomes the coordinator')
    inv = probe_ssh(args.target, args.jump)
    languages = None if args.languages == 'auto' else [s for s in args.languages.split(',') if s and s != 'none']
    native = None if args.native == 'auto' else [s for s in args.native.split(',') if s and s != 'none']
    p = plan(inv, languages, native)
    show(inv, p)
    name = (args.name or inv['hostname'].split('.')[0]).lower()
    if not name.replace('-', '').isalnum():
        raise SystemExit('use an alphanumeric node name (--name)')
    port = args.remote_port
    if p['existing_granited'] and port == 8310:
        raise SystemExit(f'{args.target} already runs granited on :8310; pass --remote-port to add a second instance')
    if not args.yes and input(f'\ninstall on {args.target} and enroll it as "{name}"? [y/N] ').strip().lower() != 'y':
        raise SystemExit('cancelled')
    ship(args.target, args.remote_dir, args.jump)
    remote = (f'cd {shlex.quote(args.remote_dir)} && python3 scripts/granitectl.py install --yes '
              f'--instance {shlex.quote(args.remote_instance)} --port {port} --host-id {shlex.quote(name)} '
              f'--languages {shlex.quote(args.languages)} --native {shlex.quote(args.native)}'
              + (' --no-service' if args.no_service else ''))
    say(f'installing on {args.target} (this streams the remote output)')
    sh(*ssh_cmd(args.target, args.jump), remote)
    keys = json.loads(sh(*ssh_cmd(args.target, args.jump),
                         f'cat {shlex.quote(args.remote_dir)}/state/secrets/keys.json', capture=True))
    local_port = free_port()
    env = 'GRANITE_PEER_' + name.upper().replace('-', '_')
    secrets = load_json(KEYS)
    secrets[env] = keys['GRANITE_ADMIN_KEY']
    write_json(KEYS, secrets, private=True)
    tunnel = ['/usr/bin/ssh', '-N', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes',
              '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
              *(['-J', args.jump] if args.jump else []),
              '-L', f'127.0.0.1:{local_port}:127.0.0.1:{port}', args.target]
    if record.get('service'):
        link = install_service(f'link-{name}', tunnel, f'granited link to {name}')
    else:
        link = f'link-{name}'
        spawn(link, tunnel)
    if not wait_healthy(local_port, 30):
        raise SystemExit(f'the {link} tunnel did not come up; see state/link-{name}.log')
    workers = admin(local_port, '/admin/models', key=keys['GRANITE_ADMIN_KEY'])['workers']
    models = [w['model'] for w in workers if not w.get('hold_reason')]
    profile = ROOT / record['profile']
    config = load_json(profile)
    config['peers'] = [peer for peer in config['peers'] if peer['id'] != name]
    config['peers'].append(dict(id=name, url=f'http://127.0.0.1:{local_port}', token_env=env, models=models))
    write_json(profile, config)
    say(f'enrolled {name} with {", ".join(models) or "no models"}; restarting the coordinator to apply it')
    restart(record)
    if not wait_healthy(record['port']):
        raise SystemExit('the coordinator did not come back; see its log')
    hosts = admin(record['port'], '/internal/hosts')['hosts']
    seen = next((h for h in hosts if h['host'] == name), {})
    say(f'{name} reachable from the coordinator: {seen.get("reachable")}')


def spawn(name, command):
    """Detached process with a recorded PID, for installs that run without a login service."""
    with open(STATE / f'{name}.log', 'ab') as log:
        child = subprocess.Popen([str(c) for c in command], cwd=ROOT, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                 start_new_session=True)
    write_json(STATE / f'{name}.process.json', dict(pid=child.pid, command=[str(c) for c in command]))
    return child.pid


def owns(current, command):
    """Whether a running command line is the process we started, so a reused PID (another
    checkout's granited, say) is never signalled. Tunnels run their saved argv; run_service execs
    `granited serve` in place, so match this checkout's path and the profile it was given."""
    if not current:
        return False
    if all(str(c) in current for c in command[1:]):
        return True
    profiles = [Path(str(c)).name for c in command if str(c).endswith('.json')]
    return str(ROOT) in current and bool(profiles) and all(p in current for p in profiles)


def stop(name):
    saved = load_json(STATE / f'{name}.process.json')
    if not saved:
        return
    current = sh('ps', '-p', saved['pid'], '-o', 'command=', check=False, capture=True) or ''
    if owns(current, saved['command']):
        os.kill(saved['pid'], 15)
        for _ in range(120):
            if not (sh('ps', '-p', saved['pid'], '-o', 'stat=', check=False, capture=True) or '').strip():
                break
            time.sleep(0.25)


def restart(record):
    if record.get('service'):
        return service_action(record['instance'], 'restart')
    saved = load_json(STATE / f'{record["instance"]}.process.json')
    stop(record['instance'])
    spawn(record['instance'], saved['command'])


def node_list(_):
    record = load_json(RECORD)
    if not record:
        raise SystemExit('no local installation record')
    for peer in load_json(ROOT / record['profile'])['peers']:
        print(f'{peer["id"]:16} {peer["url"]:28} {", ".join(peer["models"])}')


def node_remove(args):
    record = load_json(RECORD)
    profile = ROOT / record['profile']
    config = load_json(profile)
    config['peers'] = [p for p in config['peers'] if p['id'] != args.name]
    write_json(profile, config)
    if record.get('service'):
        service_action(f'link-{args.name}', 'stop')
    else:
        stop(f'link-{args.name}')
    name, path = service_paths(f'link-{args.name}')
    if platform.system() != 'Darwin':
        sh('systemctl', '--user', 'disable', name, check=False)
    path.unlink(missing_ok=True)
    restart(record)
    say(f'{args.name} removed; its own service keeps running on the node')


def scan(args):
    inv = probe_ssh(args.ssh) if args.ssh else probe_local()
    p = plan(inv)
    if args.json:
        print(json.dumps(dict(inventory=inv, plan=p), indent=2))
    else:
        show(inv, p)


APP_CONFIG = Path.home() / 'Library/Application Support/granited/app.json'


def write_app_config(instance, port, label):
    """What the menu-bar app needs to find, watch and manage this installation."""
    python = ROOT / '.venv-native/bin/python' if (ROOT / '.venv-native/bin/python').exists() else Path(sys.executable)
    write_json(APP_CONFIG, dict(url=f'http://127.0.0.1:{port}', keys=str(KEYS), label=label,
                                plist=str(service_paths(instance)[1]), logs=str(STATE), root=str(ROOT),
                                profile=str(ROOT / f'configs/{instance}.json'), python=str(python),
                                granitectl=str(ROOT / 'scripts/granitectl.py')))


def uninstall(args):
    """Stop and remove the login services this checkout installed. Models, keys and files stay."""
    record = load_json(RECORD)
    if not record:
        raise SystemExit('no local installation record')
    instances = [record['instance']] + [f'link-{p["id"]}' for p in load_json(ROOT / record['profile'], {}).get('peers', [])]
    for instance in instances:
        name, path = service_paths(instance)
        if record.get('service'):
            if platform.system() == 'Darwin':
                sh('launchctl', 'bootout', f'gui/{os.getuid()}/{name}', check=False, capture=True)
            else:
                sh('systemctl', '--user', 'disable', '--now', name, check=False, capture=True)
            if path.exists():
                path.unlink()
                say(f'removed {path}')
        else:
            stop(instance)
    if platform.system() != 'Darwin' and record.get('service'):
        sh('systemctl', '--user', 'daemon-reload', check=False)
    app = load_json(APP_CONFIG)
    if app and app.get('root') == str(ROOT):
        APP_CONFIG.unlink()
    RECORD.unlink()
    say('services removed; delete this directory to remove models, keys and environments')


def service(args):
    record = load_json(RECORD)
    if not record:
        raise SystemExit('no local installation record')
    if args.action == 'logs':
        return sh('tail', '-n', '80', STATE / f'{record["instance"]}.log')
    if record.get('service'):
        return service_action(record['instance'], args.action)
    # Installed with --no-service: a detached process with a recorded PID, no launchd/systemd unit.
    name = record['instance']
    saved = load_json(STATE / f'{name}.process.json')
    if not saved:
        raise SystemExit(f'no process record for {name}')
    running = owns(sh('ps', '-p', saved['pid'], '-o', 'command=', check=False, capture=True) or '', saved['command'])
    if args.action == 'status':
        print(f'{name}: {"running, pid " + str(saved["pid"]) if running else "stopped"} (no login service)')
    elif args.action == 'stop':
        stop(name)
    elif args.action == 'start':
        if not running:
            spawn(name, saved['command'])
    else:
        restart(record)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    s = sub.add_parser('scan')
    s.add_argument('--ssh', help='probe this SSH host instead of the local machine')
    s.add_argument('--json', action='store_true')
    s.set_defaults(func=scan)
    i = sub.add_parser('install')
    i.add_argument('--yes', action='store_true')
    i.add_argument('--instance', default='router', help='service and profile name')
    i.add_argument('--port', type=int, default=8310)
    i.add_argument('--host-id')
    i.add_argument('--languages', default='auto', help='auto, none, or e.g. 3b,8b')
    i.add_argument('--native', default='auto', help='auto, none, or e.g. embedding,timeseries')
    i.add_argument('--models-dir', default=str(ROOT / 'models'))
    i.add_argument('--no-service', action='store_true', help='run as a detached process instead of a login service')
    i.add_argument('--mac-engine', choices=['auto', 'omlx', 'llama'], default='auto',
                   help='Granite 4.2 engine on Apple silicon (auto: oMLX when installed)')
    i.set_defaults(func=install)
    n = sub.add_parser('node').add_subparsers(dest='node', required=True)
    a = n.add_parser('add')
    a.add_argument('target', help='SSH destination, e.g. user@gpu-box.local or a ~/.ssh/config alias')
    a.add_argument('--name')
    a.add_argument('--yes', action='store_true')
    a.add_argument('--remote-dir', default='granited')
    a.add_argument('--remote-port', type=int, default=8310)
    a.add_argument('--languages', default='auto')
    a.add_argument('--native', default='auto')
    a.add_argument('--no-service', action='store_true', help='run the node as a detached process')
    a.add_argument('--jump', help='reach the node through this SSH bastion (ssh -J)')
    a.add_argument('--remote-instance', default='router', help='service and profile name on the node')
    a.set_defaults(func=node_add)
    n.add_parser('list').set_defaults(func=node_list)
    r = n.add_parser('remove')
    r.add_argument('name')
    r.set_defaults(func=node_remove)
    sub.add_parser('uninstall').set_defaults(func=uninstall)
    v = sub.add_parser('service')
    v.add_argument('action', choices=['status', 'start', 'stop', 'restart', 'logs'])
    v.set_defaults(func=service)
    args = parser.parse_args()
    args.func(args)


if __name__ == '__main__':
    main()
