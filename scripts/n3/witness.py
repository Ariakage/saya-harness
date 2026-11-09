#!/usr/bin/env python3
"""Fixed-source/native-platform witnesses and fail-closed workflow outcomes."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys

from fixture import bounded_process, require, sha256

RUNNERS = {"ubuntu-22.04": ("Linux", "x86_64"), "ubuntu-22.04-arm": ("Linux", "aarch64"),
           "macos-15": ("Darwin", "arm64"), "macos-15-intel": ("Darwin", "x86_64")}
COMMON = ("source", "toolchain", "rust", "tooling", "candidate", "baseline", "performance",
          "mcp", "legacy", "plugins", "cli", "tui", "final_witness")


def command(*arguments):
    code, output, _errors, _elapsed = bounded_process(list(arguments), Path.cwd(), os.environ.copy(), timeout=30)
    require(code == 0, "witness command failed: " + arguments[0])
    return output.strip()


def source(expected):
    require(re.fullmatch(r"(?:[a-f0-9]{40}|[a-f0-9]{64})", expected), "expected source must be a complete commit object ID")
    actual = command("git", "rev-parse", "HEAD")
    dirty = command("git", "status", "--porcelain=v1", "--untracked-files=all")
    value = {"requested_sha": expected, "actual_sha": actual, "matches": actual == expected,
             "worktree_clean": not dirty, "dirty_entries": len(dirty.splitlines())}
    require(value["matches"] and value["worktree_clean"], "source differs from the selected clean commit")
    return value


def os_release():
    values = {}
    for line in Path("/etc/os-release").read_text().splitlines():
        name, separator, value = line.partition("=")
        if separator:
            values[name] = value.strip('"')
    return values


def platform_witness(label):
    require(label in RUNNERS, "unknown native runner label")
    expected = RUNNERS[label]
    actual = (platform.system(), platform.machine())
    value = {"runner_label": label, "system": actual[0], "machine": actual[1],
             "release": platform.release(), "version": platform.version()}
    require(actual == expected, "runner architecture or operating system differs from its required native target")
    if actual[0] == "Linux":
        value["distribution"] = os_release()
        value["libc"] = os.confstr("CS_GNU_LIBC_VERSION")
        require(value["distribution"].get("ID") == "ubuntu" and value["distribution"].get("VERSION_ID") == "22.04"
                and value["libc"] == "glibc 2.35", "native Ubuntu 22.04 / glibc 2.35 baseline is unavailable")
    else:
        value["macos_version"] = command("sw_vers", "-productVersion")
        require(value["macos_version"].split(".")[0] == "15", "actual macOS 15 is required")
        result = subprocess.run(["sysctl", "-n", "hw.optional.arm64"], capture_output=True, text=True, timeout=10)
        value["arm64_capable"] = result.returncode == 0 and result.stdout.strip() == "1"
        require((actual[1] == "arm64") == value["arm64_capable"], "translated execution cannot substitute for native acceptance")
    return value


def binary_witness(binary, label):
    require(binary.is_file() and not binary.is_symlink() and binary.stat().st_size <= 256 * 1024 * 1024,
            "candidate must be a bounded regular executable")
    with binary.open("rb") as image:
        header = image.read(20)
    system, machine = RUNNERS[label]
    value = {"path": str(binary.resolve()), "sha256": sha256(binary), "bytes": binary.stat().st_size,
             "version": command(str(binary.resolve()), "--version")}
    if system == "Linux":
        require(len(header) == 20 and header[:6] == b"\x7fELF\x02\x01", "expected little-endian 64-bit ELF")
        architecture = {62: "x86_64", 183: "aarch64"}.get(int.from_bytes(header[18:20], "little"))
        require(architecture == machine, "ELF architecture differs from native runner")
        symbols = command("objdump", "-T", str(binary))
        versions = re.findall(r"\bGLIBC_([0-9.]+)", symbols)
        maximum = max((tuple(map(int, version.split("."))) for version in versions), default=())
        value["maximum_glibc_symbol"] = ".".join(map(str, maximum))
        value["dependencies"] = command("ldd", str(binary))
        require(maximum and maximum <= (2, 35) and "not found" not in value["dependencies"],
                "candidate exceeds the glibc baseline or has missing dependencies")
    else:
        require(len(header) >= 8 and header[:4] == b"\xcf\xfa\xed\xfe", "expected thin little-endian 64-bit Mach-O")
        require(struct.unpack_from("<I", header, 4)[0] == (0x0100000c if machine == "arm64" else 0x01000007),
                "Mach-O architecture differs from native runner")
        require(command("lipo", "-archs", str(binary)).split() == [machine], "candidate contains an unexpected architecture")
        load = command("otool", "-l", str(binary))
        require(re.search(r"(?m)^\s*minos\s+15\.0(?:\.0)?\s*$", load), "candidate does not declare minimum macOS 15.0")
        value["mach_o_minimum"] = "15.0"
    return value


def outcomes(label, steps):
    required = list(COMMON) + (["linux_tools", "native_linux"] if label.startswith("ubuntu-") else ["native_macos"])
    observed = {name: {"outcome": item.get("outcome"), "conclusion": item.get("conclusion")}
                for name, item in steps.items()}
    missing = [name for name in required if observed.get(name, {}).get("outcome") != "success"]
    return {"status": "passed" if not missing else "failed", "required_steps": required,
            "unsatisfied_steps": missing, "steps": observed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("source", "candidate", "baseline", "final", "outcomes"))
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--runner", required=True, choices=RUNNERS)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--copy-from", type=Path)
    parser.add_argument("--expected-sha256")
    options = parser.parse_args()
    evidence = {"schema_version": 1, "phase": options.phase, "status": "failed",
                "recorded_utc": datetime.now(timezone.utc).isoformat(),
                "workflow_sha": os.environ.get("GITHUB_WORKFLOW_SHA"), "run_id": os.environ.get("GITHUB_RUN_ID"),
                "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT")}
    try:
        if options.phase == "outcomes":
            evidence.update(outcomes(options.runner, json.loads(os.environ["STEP_OUTCOMES"])))
            evidence["job_status_before_upload"] = os.environ.get("JOB_STATUS")
        else:
            evidence["source"] = source(options.source_sha)
            evidence["platform"] = platform_witness(options.runner)
            if options.phase in ("candidate", "baseline", "final"):
                require(options.binary is not None, "candidate path is required")
                if options.phase == "candidate":
                    require(options.copy_from is not None and not options.binary.exists(), "fresh candidate destination and build input are required")
                    options.binary.parent.mkdir(parents=True, exist_ok=True)
                    with options.copy_from.open("rb") as source_file, options.binary.open("xb") as destination:
                        shutil.copyfileobj(source_file, destination, 65536)
                        destination.flush()
                        os.fsync(destination.fileno())
                    options.binary.chmod(0o500)
                    require(sha256(options.copy_from) == sha256(options.binary), "candidate copy differs from the built release program")
                evidence["binary"] = binary_witness(options.binary, options.runner)
                if options.phase != "candidate":
                    require(re.fullmatch(r"[a-f0-9]{64}", options.expected_sha256 or "")
                            and evidence["binary"]["sha256"] == options.expected_sha256, "candidate changed from its original witness")
                if options.phase == "candidate":
                    evidence["toolchain"] = command("rustc", "--version", "--verbose")
                    evidence["cargo_lock_sha256"] = sha256(Path("Cargo.lock"))
                    if os.environ.get("GITHUB_OUTPUT"):
                        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                            output.write("binary=" + str(options.binary.resolve()) + "\nsha256=" + evidence["binary"]["sha256"] + "\n")
            evidence["status"] = "passed"
    except Exception as cause:
        evidence["failure"] = {"type": type(cause).__name__, "message": str(cause)[:4096]}
    options.report.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(evidence, indent=2) + "\n"
    require(len(encoded.encode()) <= 2 * 1024 * 1024, "witness report exceeds its byte bound")
    options.report.write_text(encoded)
    print(encoded, end="")
    return 0 if evidence["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
