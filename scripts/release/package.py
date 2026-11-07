#!/usr/bin/env python3
"""Create local, unsigned candidate archives from explicitly supplied binaries."""
import argparse
import gzip
import hashlib
import io
import json
import re
import struct
from pathlib import Path
import subprocess
import tarfile

TARGETS = ('linux-x86_64', 'linux-aarch64', 'macos-x86_64', 'macos-aarch64')


def git(*args):
    return subprocess.check_output(['git', *args], stderr=subprocess.DEVNULL)


def architecture_matches(target, data):
    if len(data) < 20:
        return False
    if target.startswith('linux-'):
        expected = 62 if target.endswith('x86_64') else 183
        return data[:6] == b'\x7fELF\x02\x01' and struct.unpack_from('<H', data, 18)[0] == expected
    expected = 0x01000007 if target.endswith('x86_64') else 0x0100000c
    return data[:4] == b'\xcf\xfa\xed\xfe' and struct.unpack_from('<I', data, 4)[0] == expected


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source-commit', required=True, help='commit used to build every supplied binary')
    parser.add_argument('--output', required=True, type=Path, help='new candidate directory')
    parser.add_argument('--binary', action='append', required=True, metavar='TARGET=PATH')
    args = parser.parse_args()
    try:
        if not re.fullmatch(r'[0-9a-f]{40}', args.source_commit):
            parser.error('source commit must be a full lowercase SHA-1')
        commit = git('rev-parse', '--verify', args.source_commit + '^{commit}').decode().strip()
        manifest = git('show', commit + ':Cargo.toml').decode()
        match = re.search(r'^version = \"([0-9]+\.[0-9]+\.[0-9]+)\"$', manifest, re.MULTILINE)
        if not match:
            parser.error('could not read workspace version from source commit')
        version = match.group(1)
        epoch = int(git('show', '-s', '--format=%ct', commit))
        license_data = git('show', commit + ':LICENSE')
        readme = git('show', commit + ':README.md')
        binaries = {}
        for item in args.binary:
            target, separator, path = item.partition('=')
            if not separator or target not in TARGETS or target in binaries:
                parser.error('provide each supported target exactly once')
            binaries[target] = Path(path).read_bytes()
        if set(binaries) != set(TARGETS) or any(not architecture_matches(target, data) for target, data in binaries.items()):
            parser.error('four matching 64-bit target binaries are required')
        # mkdir refuses to replace a previous candidate or its checksum file.
        args.output.mkdir(parents=True, exist_ok=False)
        checksums = []
        for target in TARGETS:
            payload = binaries[target]
            name = 'saya-' + version + '-' + target + '.tar.gz'
            info = {'schema_version': 1, 'version': version, 'target': target,
                    'declared_source_commit': commit,
                    'binary_sha256': hashlib.sha256(payload).hexdigest(),
                    'verification': 'See accompanying release validation record; this field is not a smoke-test result.'}
            contents = [('saya', payload, 0o755), ('LICENSE', license_data, 0o644),
                        ('README.md', readme, 0o644),
                        ('BUILD-INFO.json', (json.dumps(info, sort_keys=True, indent=2) + '\n').encode(), 0o644)]
            archive = args.output / name
            with archive.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode='w') as tar:
                    for item_name, data, mode in contents:
                        header = tarfile.TarInfo(item_name)
                        header.size, header.mode, header.mtime = len(data), mode, epoch
                        header.uid = header.gid = 0
                        header.uname = header.gname = ''
                        tar.addfile(header, io.BytesIO(data))
            checksums.append(hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + name)
        (args.output / 'SHA256SUMS').write_text('\n'.join(checksums) + '\n', encoding='ascii')
        print('Created four local candidate archives and SHA256SUMS in', args.output)
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print('Packaging failed:', type(error).__name__)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
