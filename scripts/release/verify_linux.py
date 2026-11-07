#!/usr/bin/env python3
"""Verify an exact Linux candidate archive on the Ubuntu 22.04 baseline."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tarfile
import tempfile

from package import architecture_matches

HERE = Path(__file__).resolve().parent


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def os_release():
    values = {}
    for line in Path('/etc/os-release').read_text().splitlines():
        key, separator, value = line.partition('=')
        if separator:
            values[key] = value.strip('"')
    return values


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('archive', type=Path)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--source-commit', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        if platform.system() != 'Linux':
            raise ValueError('this check requires Linux')
        release = os_release()
        libc = os.confstr('CS_GNU_LIBC_VERSION')
        if release.get('ID') != 'ubuntu' or release.get('VERSION_ID') != '22.04' or libc != 'glibc 2.35':
            raise ValueError('this check requires Ubuntu 22.04 with glibc 2.35')
        digest = hashlib.sha256(args.archive.read_bytes()).hexdigest()
        if digest != args.sha256.lower():
            raise ValueError('archive SHA-256 mismatch')
        with tarfile.open(args.archive, 'r:gz') as archive:
            members = archive.getmembers()
            if len(members) != 4 or {member.name for member in members} != {'saya', 'LICENSE', 'README.md', 'BUILD-INFO.json'}:
                raise ValueError('unexpected archive members')
            if any(not member.isfile() or member.size > 100 * 1024 * 1024 for member in members):
                raise ValueError('unexpected archive member type or size')
            metadata = json.load(archive.extractfile('BUILD-INFO.json'))
            target = metadata.get('target')
            native_target = {'x86_64': 'linux-x86_64', 'aarch64': 'linux-aarch64'}.get(platform.machine())
            if target not in ('linux-x86_64', 'linux-aarch64') or target != native_target:
                raise ValueError('candidate target must match the native machine')
            if metadata.get('declared_source_commit') != args.source_commit:
                raise ValueError('source commit mismatch')
            binary = archive.extractfile('saya').read()
            binary_hash = hashlib.sha256(binary).hexdigest()
            if not architecture_matches(target, binary) or binary_hash != metadata.get('binary_sha256'):
                raise ValueError('binary architecture or SHA-256 mismatch')
        with tempfile.TemporaryDirectory(prefix='saya-linux-release-') as temporary:
            path = Path(temporary) / 'saya'
            path.write_bytes(binary)
            path.chmod(0o755)
            symbols = command('objdump', '-T', str(path))
            versions = re.findall(r'\bGLIBC_([0-9.]+)', symbols)
            maximum = max((tuple(map(int, value.split('.'))) for value in versions), default=())
            if not maximum or maximum > (2, 35):
                raise ValueError('binary requires GLIBC symbols newer than 2.35 or has no GLIBC symbols')
            dependencies = command('ldd', str(path))
            if 'not found' in dependencies:
                raise ValueError('binary has missing shared libraries')
            for script in ('smoke.py', 'journey.py', 'review_regressions.py'):
                subprocess.run([sys.executable, str(HERE / script), str(path)], check=True, timeout=120)
        evidence = {
            'schema_version': 1, 'captured_at_utc': datetime.now(timezone.utc).isoformat(),
            'os': 'Linux', 'distribution': release, 'libc': libc, 'machine': platform.machine(),
            'kernel': platform.release(), 'target': target, 'archive_name': args.archive.name,
            'archive_sha256': digest, 'binary_sha256': binary_hash,
            'declared_source_commit': metadata['declared_source_commit'],
            'maximum_glibc_symbol': '.'.join(map(str, maximum)), 'dependencies': dependencies,
            'rustc': command('rustc', '--version'), 'offline_tool_smoke': 'passed',
            'interactive_journey': 'passed', 'review_regressions': 'passed',
            'test_script_sha256': {name: hashlib.sha256((HERE / name).read_bytes()).hexdigest()
                                   for name in ('smoke.py', 'journey.py', 'review_regressions.py')},
        }
        with args.output.open('x', encoding='utf-8') as output:
            json.dump(evidence, output, indent=2, sort_keys=True)
            output.write('\n')
        print('Saved Linux baseline evidence:', args.output)
        return 0
    except (OSError, ValueError, KeyError, tarfile.TarError, json.JSONDecodeError,
            subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print('Verification failed:', str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
