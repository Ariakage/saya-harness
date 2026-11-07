#!/usr/bin/env python3
"""Verify an existing candidate on macOS 15 and save a bounded evidence record."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import struct
import sys
import tarfile
import tempfile

HERE = Path(__file__).resolve().parent


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('archive', type=Path)
    parser.add_argument('--sha256', required=True, help='expected archive SHA-256 from SHA256SUMS')
    parser.add_argument('--output', type=Path, required=True, help='new evidence JSON file')
    args = parser.parse_args()
    try:
        if platform.system() != 'Darwin':
            raise ValueError('this check must run on macOS 15')
        version = command('sw_vers', '-productVersion')
        if version.split('.')[0] != '15':
            raise ValueError('this check requires macOS 15; found ' + version)
        digest = hashlib.sha256(args.archive.read_bytes()).hexdigest()
        if digest != args.sha256.lower():
            raise ValueError('archive SHA-256 mismatch')
        with tarfile.open(args.archive, 'r:gz') as archive:
            members = archive.getmembers()
            if len(members) != 4 or {item.name for item in members} != {'saya', 'LICENSE', 'README.md', 'BUILD-INFO.json'}:
                raise ValueError('unexpected archive members')
            if any(not item.isfile() or item.size > 100 * 1024 * 1024 for item in members):
                raise ValueError('unexpected archive member type or size')
            metadata = json.load(archive.extractfile('BUILD-INFO.json'))
            target = metadata.get('target')
            if target not in ('macos-aarch64', 'macos-x86_64'):
                raise ValueError('candidate is not a macOS target')
            binary = archive.extractfile('saya').read()
            binary_hash = hashlib.sha256(binary).hexdigest()
            expected_cpu = 0x0100000c if target == 'macos-aarch64' else 0x01000007
            if len(binary) < 8 or binary[:4] != b'\xcf\xfa\xed\xfe' or struct.unpack_from('<I', binary, 4)[0] != expected_cpu:
                raise ValueError('binary architecture does not match target')
            if binary_hash != metadata.get('binary_sha256'):
                raise ValueError('binary SHA-256 mismatch')
        try:
            arm64_capable = command('sysctl', '-n', 'hw.optional.arm64') == '1'
        except subprocess.CalledProcessError:
            arm64_capable = False
        machine = platform.machine()
        native_target = 'macos-aarch64' if machine == 'arm64' and arm64_capable else None
        if machine == 'x86_64' and not arm64_capable:
            native_target = 'macos-x86_64'
        if target != native_target:
            raise ValueError('candidate target must match the native machine without translation')
        with tempfile.TemporaryDirectory(prefix='saya-macos15-') as temporary:
            path = Path(temporary) / 'saya'
            path.write_bytes(binary)
            path.chmod(0o755)
            build_info = command('otool', '-l', str(path))
            if not re.search(r'(?m)^\s*minos\s+15\.0(?:\.0)?\s*$', build_info):
                raise ValueError('Mach-O does not declare minimum macOS 15.0')
            for script in ('smoke.py', 'journey.py', 'review_regressions.py', 'tui_journey.py'):
                subprocess.run([sys.executable, str(HERE / script), str(path)], check=True, timeout=120)
        evidence = {
            'schema_version': 1, 'captured_at_utc': datetime.now(timezone.utc).isoformat(),
            'macos_version': version, 'machine': machine, 'arm64_capable': arm64_capable,
            'target': target, 'archive_name': args.archive.name, 'archive_sha256': digest,
            'binary_sha256': binary_hash, 'declared_source_commit': metadata.get('declared_source_commit'),
            'mach_o_minimum': '15.0', 'offline_tool_smoke': 'passed', 'interactive_journey': 'passed',
            'review_regressions': 'passed', 'tui_journey': 'passed',
            'smoke_script_sha256': hashlib.sha256((HERE / 'smoke.py').read_bytes()).hexdigest(),
            'journey_script_sha256': hashlib.sha256((HERE / 'journey.py').read_bytes()).hexdigest(),
            'tui_journey_script_sha256': hashlib.sha256((HERE / 'tui_journey.py').read_bytes()).hexdigest(),
            'review_regressions_script_sha256': hashlib.sha256((HERE / 'review_regressions.py').read_bytes()).hexdigest(),
        }
        with args.output.open('x', encoding='utf-8') as output:
            json.dump(evidence, output, indent=2, sort_keys=True)
            output.write('\n')
        print('Saved macOS 15 evidence:', args.output)
        return 0
    except (OSError, ValueError, KeyError, tarfile.TarError, json.JSONDecodeError,
            subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print('Verification failed:', str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
