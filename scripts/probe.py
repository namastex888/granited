#!/usr/bin/env python3
"""Read-only inventory of one machine: OS, memory, accelerators, NPUs, tools and running engines.

Stdlib only and Python 3.8 compatible, so `ssh HOST python3 - < probe.py` works on a fresh node.
It starts nothing, installs nothing and never loads a model.
"""
import json
import os
import platform
import shutil
import subprocess
import urllib.request

GIB = 1024 ** 3


def run(*cmd, timeout=10):
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return out.stdout.strip() if out.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def read(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return None


def get(url, timeout=1.5):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return json.loads(r.read().decode() or 'null')
    except Exception:
        return None


def memory():
    if platform.system() == 'Darwin':
        total = int(run('sysctl', '-n', 'hw.memsize') or 0)
        pages = {}
        for line in (run('vm_stat') or '').splitlines()[1:]:
            key, _, value = line.partition(':')
            pages[key.strip()] = int(value.strip().rstrip('.') or 0)
        size = int(run('sysctl', '-n', 'hw.pagesize') or 16384)
        available = size * sum(pages.get(k, 0) for k in ('Pages free', 'Pages inactive', 'Pages speculative', 'Pages purgeable'))
        swap = run('sysctl', '-n', 'vm.swapusage') or ''
        return dict(total_bytes=total, available_bytes=available, swap=swap)
    info = {}
    for line in (read('/proc/meminfo') or '').splitlines():
        key, _, value = line.partition(':')
        info[key] = int(value.split()[0]) * 1024 if value.split() else 0
    psi = read('/proc/pressure/memory') or ''
    full = next((f.split('=')[1] for line in psi.splitlines() if line.startswith('full') for f in line.split() if f.startswith('avg10=')), None)
    return dict(total_bytes=info.get('MemTotal', 0), available_bytes=info.get('MemAvailable', 0),
                swap_total_bytes=info.get('SwapTotal', 0), swap_used_bytes=info.get('SwapTotal', 0) - info.get('SwapFree', 0),
                memory_stall_percent=float(full) if full else None)


def gpus():
    found = []
    csv = run('nvidia-smi', '--query-gpu=name,memory.total,memory.free,driver_version,compute_cap', '--format=csv,noheader,nounits')
    for line in (csv or '').splitlines():
        f = [x.strip() for x in line.split(',')]
        if len(f) == 5:
            found.append(dict(kind='nvidia', name=f[0], memory_bytes=int(float(f[1])) * 1024 ** 2,
                              free_bytes=int(float(f[2])) * 1024 ** 2, driver=f[3], compute_capability=f[4]))
    if platform.system() == 'Darwin':
        data = json.loads(run('system_profiler', 'SPDisplaysDataType', '-json', timeout=20) or '{}')
        for d in data.get('SPDisplaysDataType', []):
            found.append(dict(kind='apple', name=d.get('sppci_model'), cores=d.get('sppci_cores'),
                              metal=d.get('spdisplays_mtlgpufamilysupport'), memory='unified'))
        return found
    vendors = {'0x1002': 'amd', '0x8086': 'intel', '0x10de': 'nvidia'}
    for card in sorted(os.listdir('/sys/class/drm')) if os.path.isdir('/sys/class/drm') else []:
        if not card.startswith('card') or '-' in card:
            continue
        base = '/sys/class/drm/%s/device' % card
        vendor = vendors.get(read(base + '/vendor'))
        if vendor in ('amd', 'intel'):
            vram = read(base + '/mem_info_vram_total')
            found.append(dict(kind=vendor, node=card, device_id=read(base + '/device'),
                              driver=os.path.basename(os.path.realpath(base + '/driver')),
                              dedicated_vram_bytes=int(vram) if vram else None))
    icds = []
    for d in ('/usr/share/vulkan/icd.d', '/etc/vulkan/icd.d'):
        icds += [f for f in (os.listdir(d) if os.path.isdir(d) else []) if f.endswith('.json')]
    for gpu in found:
        if gpu['kind'] in ('amd', 'intel'):
            gpu['vulkan_icd'] = any(('radeon' if gpu['kind'] == 'amd' else 'intel') in f for f in icds)
    vk = run('vulkaninfo', '--summary', timeout=15)
    if vk:
        names = [l.split('=', 1)[1].strip() for l in vk.splitlines() if 'deviceName' in l]
        types = [l.split('=', 1)[1].strip() for l in vk.splitlines() if 'deviceType' in l]
        for gpu in found:
            gpu['vulkan'] = any(t != 'PHYSICAL_DEVICE_TYPE_CPU' for t in types)
        found.append(dict(kind='vulkan-summary', devices=[dict(name=n, type=t) for n, t in zip(names, types)]))
    return found


def npus():
    found = []
    base = '/sys/class/accel'
    for node in sorted(os.listdir(base)) if os.path.isdir(base) else []:
        driver = os.path.basename(os.path.realpath('%s/%s/device/driver' % (base, node)))
        entry = dict(node=node, driver=driver, firmware=read('%s/%s/device/fw_version' % (base, node)))
        if driver == 'amdxdna':
            entry['kind'] = 'amd-xdna'
            examine = run('xrt-smi', 'examine', timeout=15) or ''
            entry['xrt'] = next((l.split(':', 1)[1].strip() for l in examine.splitlines() if l.strip().startswith('Version')), None)
            entry['device'] = next((l.split('|')[2].strip() for l in examine.splitlines() if 'npu' in l.lower() and l.count('|') >= 4), None)
        elif driver == 'intel_vpu':
            entry['kind'] = 'intel-npu'
        found.append(entry)
    return found


def engines():
    """Engines already serving on loopback. Reported only; granited never adopts them implicitly."""
    seen = []
    checks = [
        ('granited', 8310, '/healthz'), ('llama.cpp', 8080, '/props'), ('llama.cpp', 8321, '/props'),
        ('ollama', 11434, '/api/tags'), ('lemonade', 13305, '/api/v1/models'), ('lemonade', 8000, '/api/v1/models'),
        ('lm-studio', 1234, '/v1/models'), ('vllm', 8000, '/v1/models'), ('omlx', 8000, '/admin/api/models'),
    ]
    for name, port, path in checks:
        data = get('http://127.0.0.1:%d%s' % (port, path))
        if data is None:
            continue
        models = []
        if isinstance(data, dict):
            items = data.get('data') or data.get('models') or []
            models = [m.get('id') or m.get('name') or m.get('model') for m in items if isinstance(m, dict)]
        seen.append(dict(engine=name, port=port, models=[m for m in models if m]))
    installed = {n: shutil.which(n) for n in ('llama-server', 'lemonade-server', 'flm', 'ollama', 'docker', 'podman')}
    home = os.path.expanduser('~')
    for name, path in (('omlx', home + '/.omlx/bin/omlx'), ('lemonade-server', '/usr/bin/lemonade-server')):
        if not installed.get(name) and os.path.exists(path):
            installed[name] = path
    for package in ('lemonade-server', 'fastflowlm'):
        version = run('dpkg-query', '-W', '-f=${Version}', package)
        if version:
            installed[package + ' (deb)'] = version
    return dict(running=seen, installed={k: v for k, v in installed.items() if v})


def tools():
    home = os.path.expanduser('~')
    cargo = shutil.which('cargo') or (home + '/.cargo/bin/cargo' if os.path.exists(home + '/.cargo/bin/cargo') else None)
    docker_gpu = None
    if shutil.which('docker'):
        info = run('docker', 'info', '--format', '{{json .Runtimes}}', timeout=10)
        docker_gpu = bool(info and 'nvidia' in info)
    return dict(python=platform.python_version(), cargo=cargo, uv=shutil.which('uv'), git=shutil.which('git'),
                curl=shutil.which('curl'), brew=shutil.which('brew'), systemd=bool(shutil.which('systemctl')),
                launchd=platform.system() == 'Darwin', docker_nvidia_runtime=docker_gpu,
                linger=read('/var/lib/systemd/linger/' + os.environ.get('USER', '')) is not None
                if os.path.isdir('/var/lib/systemd/linger') else None)


def main():
    usage = shutil.disk_usage(os.path.expanduser('~'))
    os_release = dict(l.split('=', 1) for l in (read('/etc/os-release') or '').splitlines() if '=' in l)
    print(json.dumps(dict(
        schema=1, hostname=platform.node(), system=platform.system(), machine=platform.machine(),
        release=platform.release(), distro=os_release.get('PRETTY_NAME', '').strip('"') or platform.mac_ver()[0],
        cpu=run('sysctl', '-n', 'machdep.cpu.brand_string') or next(
            (l.split(':', 1)[1].strip() for l in (read('/proc/cpuinfo') or '').splitlines() if l.startswith('model name')), None),
        logical_cpus=os.cpu_count(), memory=memory(), disk_free_bytes=usage.free,
        gpus=gpus(), npus=npus(), engines=engines(), tools=tools(),
    ), indent=1))


if __name__ == '__main__':
    main()
