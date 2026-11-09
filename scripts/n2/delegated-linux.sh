#!/bin/sh
# Run within a systemd service explicitly delegated to this user.
set -eu
[ "$(uname -s)" = Linux ]
relative=
while IFS=: read -r hierarchy controllers path; do
  if [ "$hierarchy" = 0 ] && [ -z "$controllers" ]; then relative=$path; fi
done < /proc/self/cgroup
[ -n "$relative" ]
case "$relative" in /*) ;; *) exit 2 ;; esac
root="/sys/fs/cgroup$relative"
[ -w "$root/cgroup.procs" ]
mkdir "$root/supervisor"
printf '%s\n' "$$" > "$root/supervisor/cgroup.procs"
printf '%s\n' '+cpu +memory +pids' > "$root/cgroup.subtree_control"
mkdir "$root/control"
printf '%s\n' '+cpu +memory +pids' > "$root/control/cgroup.subtree_control"
export SAYA_N2_CGROUP_ROOT="$root/control"
export SAYA_TEST_CGROUP_ROOT="$root/control"
export PYTHONDONTWRITEBYTECODE=1
exec /usr/bin/python3 scripts/n2/native_acceptance.py "$@"
