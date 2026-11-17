#!/bin/sh
# Offline Python tooling regressions shared by `just test-tooling`, the CI
# workspace job and N5 validation. Pass `-v`/`--verbose` to forward unittest
# verbosity to every suite.
set -eu
cd "$(dirname "$0")/.."

verbose=
case "${1:-}" in
    -v|--verbose) verbose=-v ;;
    "") ;;
    *) echo "usage: $0 [-v|--verbose]" >&2; exit 2 ;;
esac

python3 -B scripts/test_prompt_eval.py $verbose
python3 -B scripts/n2/test_install_linux_tools.py $verbose
python3 -B scripts/n2/test_native_acceptance.py $verbose
python3 -B scripts/n2/test_verify_performance.py $verbose
python3 -B scripts/release/test_package.py $verbose
python3 -B scripts/release/test_verify_macos15.py $verbose
python3 -B scripts/release/test_verify_linux.py $verbose
python3 -B scripts/release/test_script_helpers.py $verbose
python3 -B -m unittest discover -s scripts/n3 -p 'test_*.py' $verbose
python3 -B scripts/n5/test_browser_vm.py $verbose
python3 -I -B scripts/n5/test_acceptance.py $verbose
python3 -B scripts/n5/test_witness.py $verbose
