# helper sourced by suites: flag <NAME> -> 1/0 (default 0)
flag() {
  local f="$WORK/upstream.flags"
  [ -f "$f" ] && grep -q "^$1=1$" "$f" && echo 1 || echo 0
}
