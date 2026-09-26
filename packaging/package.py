#!/usr/bin/env python3
"""Package already-built static binaries. Python 3.11+, dpkg-deb and rpmbuild.

No credentials, network installer, cross-compiler or native package framework is
hidden here. The same binary is used in the archive, deb, rpm and container.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    'x86_64-unknown-linux-musl': ('amd64', 'x86_64', 62),
    'aarch64-unknown-linux-musl': ('arm64', 'aarch64', 183),
}


def run(*args, **kwargs):
    return subprocess.check_output([str(a) for a in args], text=True, **kwargs).strip()


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def versions(value):
    match = re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*))?', value)
    if not match:
        raise ValueError('Expected SemVer X.Y.Z[-prerelease]; build metadata is not supported')
    base = '.'.join(match.groups()[:3])
    return base + ('~' + match[4] if match[4] else '')


def version():
    value = tomllib.loads((ROOT / 'Cargo.toml').read_text())['package']['version']
    versions(value)
    return value


def check_elf(path, target):
    data = Path(path).read_bytes()
    if len(data) < 64 or data[:7] != b'\x7fELF\x02\x01\x01':
        raise ValueError('Expected a little-endian ELF64 binary')
    _, _, machine = TARGETS[target]
    if struct.unpack_from('<H', data, 18)[0] != machine:
        raise ValueError('ELF architecture does not match the package target')
    if struct.unpack_from('<H', data, 16)[0] not in (2, 3):
        raise ValueError('ELF is not an executable')
    offset = struct.unpack_from('<Q', data, 32)[0]
    size, count = struct.unpack_from('<HH', data, 54)
    if size != 56 or not 1 <= count <= 4096 or offset + size * count > len(data):
        raise ValueError('Invalid ELF program headers')
    for i in range(count):
        kind, _, start, _, _, length, _, _ = struct.unpack_from('<IIQQQQQQ', data, offset + size * i)
        if kind == 3:
            raise ValueError('Dynamic interpreter found: not a portable static binary')
        if kind == 2:
            if start + length > len(data) or length % 16:
                raise ValueError('Invalid ELF dynamic section')
            for position in range(start, start + length, 16):
                tag, _ = struct.unpack_from('<qQ', data, position)
                if tag == 0:
                    break
                if tag == 1:
                    raise ValueError('DT_NEEDED found: dynamic library dependency forbidden')
    return {'format': 'ELF64', 'target': target, 'interpreter': None, 'needed_libraries': []}


def copy(source, destination, mode=0o644):
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def checksums(directory, filename='SHA256SUMS'):
    directory = Path(directory)
    paths = sorted(p for p in directory.rglob('*') if p.is_file() and p.name != filename)
    (directory / filename).write_text(''.join(f'{sha256(p)}  {p.relative_to(directory).as_posix()}\n' for p in paths))


def archive(source, output, epoch):
    with Path(output).open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode='w', format=tarfile.PAX_FORMAT) as tar:
            for p in [source] + sorted(source.rglob('*')):
                if p.is_symlink() or not (p.is_file() or p.is_dir()):
                    raise ValueError(f'Non-regular archive input: {p}')
                info = tar.gettarinfo(str(p), arcname=p.relative_to(source.parent).as_posix())
                info.uid = info.gid = 0
                info.uname = info.gname = 'root'
                info.mtime = epoch
                info.pax_headers = {}
                if p.is_file():
                    with p.open('rb') as stream:
                        tar.addfile(info, stream)
                else:
                    tar.addfile(info)


def notices(destination):
    metadata = json.loads(run('cargo', 'metadata', '--locked', '--format-version=1', cwd=ROOT))
    sections = []
    inventory = []
    for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
        if package['name'] == 'pay-lmm':
            continue
        inventory.append({k: package.get(k) for k in ('name', 'version', 'license', 'source', 'repository')})
        sections.append(f"\n{'=' * 72}\n{package['name']} {package['version']}\nDeclared license: {package.get('license')}\nSource: {package.get('source')}\n")
        folder = Path(package['manifest_path']).parent
        files = {p for pattern in ('LICENSE*', 'LICENCE*', 'COPYING*', 'NOTICE*', 'COPYRIGHT*') for p in folder.glob(pattern) if p.is_file()}
        if package.get('license_file'):
            candidate = folder / package['license_file']
            if candidate.is_file():
                files.add(candidate)
        for p in sorted(files):
            if p.stat().st_size <= 1024 * 1024:
                sections.append(f'\n--- {p.name} ---\n' + p.read_text(errors='replace'))
    (destination / 'THIRD-PARTY-NOTICES.txt').write_text('Automatically collected dependency notices; includes build/test dependencies.\n' + '\n'.join(sections))
    (destination / 'dependencies.json').write_text(json.dumps(inventory, indent=2, sort_keys=True) + '\n')


def payload(destination, binary, info):
    destination.mkdir(parents=True)
    copy(binary, destination / 'pay-lmm', 0o755)
    copy(ROOT / 'packaging/install.sh', destination / 'install.sh', 0o755)
    for name in ('README.md', 'LICENSE', 'SECURITY.md'):
        copy(ROOT / name, destination / name)
    for directory in ('docs', 'examples', 'proto', 'deploy'):
        for p in sorted((ROOT / directory).rglob('*')):
            if p.is_file():
                copy(p, destination / p.relative_to(ROOT), 0o755 if p.suffix == '.openrc' else 0o644)
    copy(ROOT / 'packaging/maintainer.sh', destination / 'deploy/maintainer.sh', 0o755)
    # Installed services use an absolute persistent data path, unlike dev examples.
    config = (destination / 'examples/config.toml').read_text()
    config = config.replace('database = "data/pay.sqlite3"', 'database = "/var/lib/pay-lmm/pay.sqlite3"')
    (destination / 'examples/config.toml').write_text(config)
    (destination / 'ARCH').write_text(info['architecture'] + '\n')
    (destination / 'build-info.json').write_text(json.dumps(info, indent=2, sort_keys=True) + '\n')
    notices(destination)
    checksums(destination)


def native_root(source, root):
    root.mkdir()
    copy(source / 'pay-lmm', root / 'usr/bin/pay-lmm', 0o755)
    for p in sorted(source.rglob('*')):
        if p.is_file() and p.relative_to(source).as_posix() not in ('pay-lmm', 'install.sh', 'SHA256SUMS', 'ARCH'):
            copy(p, root / 'usr/share/doc/pay-lmm' / p.relative_to(source))
    copy(source / 'deploy/pay-lmm.service', root / 'usr/lib/systemd/system/pay-lmm.service')
    copy(source / 'deploy/maintainer.sh', root / 'usr/lib/pay-lmm/maintainer.sh', 0o755)


RELOAD = '''if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload || true
fi
'''


def deb(source, work, output, v, release, arch, epoch):
    root = work / 'deb-root'
    native_root(source, root)
    control = root / 'DEBIAN'
    control.mkdir()
    size = sum(p.stat().st_size for p in root.rglob('*') if p.is_file()) // 1024 + 1
    (control / 'control').write_text(f'''Package: pay-lmm
Version: {versions(v)}-{release}
Section: web
Priority: optional
Architecture: {arch}
Maintainer: TokenNotIncluded <lightjunction.me@gmail.com>
Homepage: https://github.com/TokenNotIncluded/pay.lmm.best
License: MIT
Depends: passwd
Installed-Size: {size}
Description: Payment gateway protocol aggregation service
 Rust service connecting existing payment gateways. No acquiring,
 payment processing, funds custody, settlement or payouts.
''')
    scripts = {
        'postinst': 'if [ "${1:-}" = configure ]; then /usr/lib/pay-lmm/maintainer.sh setup; fi\n',
        'prerm': 'case "${1:-}" in remove|deconfigure) /usr/lib/pay-lmm/maintainer.sh stop ;; esac\n',
        'postrm': RELOAD,
    }
    for name, body in scripts.items():
        (control / name).write_text('#!/bin/sh\nset -eu\n' + body)
        (control / name).chmod(0o755)
    for p in root.rglob('*'):
        os.utime(p, (epoch, epoch))
    path = output / f'pay-lmm_{v}-{release}_{arch}.deb'
    subprocess.run(['dpkg-deb', '-Zxz', '-z9', '--root-owner-group', '--build', str(root), str(path)], check=True)
    return path


def rpm(source, work, output, v, release, arch, epoch):
    root = work / 'rpm-root'
    native_root(source, root)
    top = work / 'rpm'
    for directory in ('BUILD', 'BUILDROOT', 'RPMS', 'SOURCES', 'SPECS', 'SRPMS'):
        (top / directory).mkdir(parents=True)
    spec = top / 'SPECS/pay-lmm.spec'
    spec.write_text(f'''%global debug_package %{{nil}}
%global __os_install_post %{{nil}}
%global _build_id_links none
%global _binary_payload w9.xzdio
Name: pay-lmm
Version: {versions(v)}
Release: {release}
Summary: Payment gateway protocol aggregation service
License: MIT
URL: https://github.com/TokenNotIncluded/pay.lmm.best
BuildArch: {arch}
AutoReqProv: no
Requires: /bin/sh
Requires(post): /usr/sbin/useradd
Requires(post): /usr/sbin/groupadd
%description
Rust protocol aggregation only. No payment processing, acquiring, funds
custody, settlement or payouts. Actual checkout belongs to upstream providers.
%prep
%build
%install
mkdir -p "%{{buildroot}}"
cp -a "{root}/." "%{{buildroot}}/"
%post
set -eu
/usr/lib/pay-lmm/maintainer.sh setup
%preun
set -eu
if [ "$1" = 0 ]; then /usr/lib/pay-lmm/maintainer.sh stop; fi
%postun
{RELOAD}
%files
%defattr(-,root,root,-)
/usr/bin/pay-lmm
/usr/lib/pay-lmm
/usr/lib/systemd/system/pay-lmm.service
%doc /usr/share/doc/pay-lmm
''')
    subprocess.run(['rpmbuild', '-bb', '--target', arch, '--define', f'_topdir {top}', '--define', '_buildhost reproducible', '--define', 'use_source_date_epoch_as_buildtime 1', '--define', 'clamp_mtime_to_source_date_epoch 1', str(spec)], check=True)
    candidates = list((top / 'RPMS').rglob('*.rpm'))
    if len(candidates) != 1:
        raise ValueError('Expected one RPM output')
    path = output / f'pay-lmm-{v}-{release}.{arch}.rpm'
    shutil.copyfile(candidates[0], path)
    return path


def names(v, arch, release=1):
    rpm_arch = 'x86_64' if arch == 'amd64' else 'aarch64'
    return [f'pay-lmm-v{v}-linux-{arch}.tar.gz', f'pay-lmm_{v}-{release}_{arch}.deb', f'pay-lmm-{v}-{release}.{rpm_arch}.rpm', f'pay-lmm-v{v}-linux-{arch}.build.json']


def build(args):
    arch, rpm_arch, _ = TARGETS[args.target]
    binary = (args.binary or ROOT / 'target' / args.target / 'release/pay-lmm').resolve()
    v = version()
    elf = check_elf(binary, args.target)
    if run(binary, '--version') != f'pay-lmm {v}':
        raise ValueError('Cargo.toml and executable versions disagree')
    ref = os.environ.get('GITHUB_REF', '')
    if ref.startswith('refs/tags/') and ref != f'refs/tags/v{v}':
        raise ValueError('Release tag must exactly match Cargo.toml version')
    epoch = int(os.environ.get('SOURCE_DATE_EPOCH') or run('git', 'log', '-1', '--format=%ct', cwd=ROOT))
    if epoch < 0:
        raise ValueError('Invalid SOURCE_DATE_EPOCH')
    os.environ['SOURCE_DATE_EPOCH'] = str(epoch)
    info = {
        'name': 'pay-lmm', 'version': v, 'package_release': args.package_release,
        'architecture': arch, 'elf': elf, 'source_date_epoch': epoch,
        'revision': run('git', 'rev-parse', 'HEAD', cwd=ROOT),
        'dirty': bool(run('git', 'status', '--porcelain', '--untracked-files=no', cwd=ROOT)),
        'rustc': run('rustc', '--version'), 'cargo': run('cargo', '--version'),
        'cargo_lock_sha256': sha256(ROOT / 'Cargo.lock'),
        'binary_sha256': sha256(binary), 'binary_bytes': binary.stat().st_size,
        'build_script_sha256': sha256(ROOT / 'packaging/build.sh'),
    }
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    generated = []
    with tempfile.TemporaryDirectory(prefix='pay-lmm-package-') as temporary:
        work = Path(temporary)
        source = work / f'pay-lmm-v{v}-linux-{arch}'
        payload(source, binary, info)
        if 'tar' in args.formats:
            path = output / names(v, arch)[0]
            archive(source, path, epoch)
            # Check archive determinism before accepting it as a release asset.
            again = work / 'repeat.tar.gz'
            archive(source, again, epoch)
            if sha256(path) != sha256(again):
                raise ValueError('Archive is not reproducible')
            generated.append(path)
        if 'deb' in args.formats:
            generated.append(deb(source, work, output, v, args.package_release, arch, epoch))
        if 'rpm' in args.formats:
            generated.append(rpm(source, work, output, v, args.package_release, rpm_arch, epoch))
        manifest = output / names(v, arch)[3]
        shutil.copyfile(source / 'build-info.json', manifest)
        generated.append(manifest)
    copy(binary, output / f'container/{arch}/pay-lmm', 0o755)
    (output / f'checksums-{arch}.txt').write_text(''.join(f'{sha256(p)}  {p.name}\n' for p in sorted(generated)))
    print(json.dumps({'version': v, 'target': args.target, 'assets': [p.name for p in generated]}, indent=2))


def assemble(directory):
    """Verify both build jobs, make a combined manifest, stage exact image binaries."""
    v = version()
    revision = run('git', 'rev-parse', 'HEAD', cwd=ROOT)
    all_files = []
    for target, (arch, _, _) in TARGETS.items():
        required = set(names(v, arch))
        lines = (directory / f'checksums-{arch}.txt').read_text().splitlines()
        seen = set()
        for line in lines:
            match = re.fullmatch(r'([0-9a-f]{64})  ([A-Za-z0-9_.-]+)', line)
            if not match or match[2] not in required or match[2] in seen:
                raise ValueError('Invalid artifact checksum manifest')
            if sha256(directory / match[2]) != match[1]:
                raise ValueError('Artifact checksum mismatch')
            seen.add(match[2])
        if seen != required:
            raise ValueError('Incomplete architecture artifact set')
        info = json.loads((directory / names(v, arch)[3]).read_text())
        if info['revision'] != revision or info['version'] != v or info['dirty'] or info['package_release'] != 1 or info['elf']['target'] != target:
            raise ValueError('Artifact provenance does not match this release commit')
        with tarfile.open(directory / names(v, arch)[0], 'r:gz') as tar:
            member = tar.getmember(f'pay-lmm-v{v}-linux-{arch}/pay-lmm')
            if not member.isfile():
                raise ValueError('Expected a regular executable in archive')
            stream = tar.extractfile(member)
            path = directory / f'container/{arch}/pay-lmm'
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open('wb') as destination:
                shutil.copyfileobj(stream, destination)
            path.chmod(0o755)
        check_elf(path, target)
        if sha256(path) != info['binary_sha256']:
            raise ValueError('Archive executable does not match build manifest')
        all_files.extend(directory / name for name in sorted(required))
    (directory / 'SHA256SUMS').write_text(''.join(f'{sha256(p)}  {p.name}\n' for p in sorted(all_files)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('version')
    check = commands.add_parser('check')
    check.add_argument('--target', choices=TARGETS, required=True)
    check.add_argument('--binary', type=Path, required=True)
    pack = commands.add_parser('build')
    pack.add_argument('--target', choices=TARGETS, required=True)
    pack.add_argument('--binary', type=Path)
    pack.add_argument('--output', type=Path, default=ROOT / 'dist')
    pack.add_argument('--package-release', type=int, choices=range(1, 100), default=1)
    pack.add_argument('--formats', choices=('tar', 'deb', 'rpm'), nargs='+', default=['tar', 'deb', 'rpm'])
    combine = commands.add_parser('assemble')
    combine.add_argument('--directory', type=Path, default=ROOT / 'dist')
    args = parser.parse_args()
    if args.command == 'version':
        print(version())
    elif args.command == 'check':
        print(json.dumps(check_elf(args.binary, args.target)))
    elif args.command == 'build':
        build(args)
    else:
        assemble(args.directory.resolve())


if __name__ == '__main__':
    main()
