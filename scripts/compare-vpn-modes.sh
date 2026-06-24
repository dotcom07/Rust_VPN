#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
WG_NAME="${WG_NAME:-wg0}"
WG_CONF="${WG_CONF:-$ROOT/config/wireguard/$WG_NAME.conf}"
LITEVPN_CONFIG="${LITEVPN_CONFIG:-$ROOT/config/client.toml}"
DURATION="${DURATION:-10}"
PARALLEL="${PARALLEL:-1}"
OUT_DIR="${OUT_DIR:-bench-results}"
RUN_MODES="${RUN_MODES:-wireguard litevpn}"
WG_QUICK_BIN="${WG_QUICK_BIN:-}"
LITEVPN_CONNECT_RETRIES="${LITEVPN_CONNECT_RETRIES:-3}"
LITEVPN_CONNECT_RETRY_DELAY_MS="${LITEVPN_CONNECT_RETRY_DELAY_MS:-1000}"
FASTCOM_PAUSE="${FASTCOM_PAUSE:-0}"
OPEN_FASTCOM="${OPEN_FASTCOM:-1}"
FASTCOM_URL="${FASTCOM_URL:-https://fast.com/ko/#}"
PREFLIGHT=0
CLI_RUN_MODES=""

LITEVPN_PID=""
LOCAL_WG_UP=0
SUDO_KEEPALIVE_PID=""
STAMP="$(date +%Y%m%d-%H%M%S)"
COMPARE_DIR="$ROOT/$OUT_DIR/vpn-compare-$STAMP"
SUMMARY="$COMPARE_DIR/summary.csv"

usage() {
  cat <<'HELP'
Usage:
  scripts/compare-vpn-modes.sh --preflight
  scripts/compare-vpn-modes.sh --mode wireguard --mode litevpn
  scripts/compare-vpn-modes.sh --mode wireguard --fastcom
  HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/compare-vpn-modes.sh

Checks prerequisites without local sudo when run with --preflight.
Runs WireGuard and LiteVPN sequentially, benchmarks each tunnel with iperf3,
and writes logs under bench-results/vpn-compare-*/.

Environment:
  RUN_MODES="wireguard litevpn"
  DURATION=10
  PARALLEL=1
  WG_QUICK_BIN=/opt/homebrew/bin/wg-quick
  WG_CONF=config/wireguard/wg0.conf
  LITEVPN_CONFIG=config/client.toml
  LITEVPN_CONNECT_RETRIES=3
  LITEVPN_CONNECT_RETRY_DELAY_MS=1000
  FASTCOM_PAUSE=1  # pause after each mode for manual Fast.com loaded-latency capture
  OPEN_FASTCOM=1
  FASTCOM_URL=https://fast.com/ko/#
HELP
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --preflight)
      PREFLIGHT=1
      shift
      ;;
    --fastcom)
      FASTCOM_PAUSE=1
      shift
      ;;
    --no-open-fastcom)
      OPEN_FASTCOM=0
      shift
      ;;
    --mode)
      if [[ -z "${2:-}" ]]; then
        echo "--mode requires wireguard or litevpn" >&2
        exit 1
      fi
      CLI_RUN_MODES="${CLI_RUN_MODES:+$CLI_RUN_MODES }$2"
      shift 2
      ;;
    --mode=*)
      CLI_RUN_MODES="${CLI_RUN_MODES:+$CLI_RUN_MODES }${1#*=}"
      shift
      ;;
    wireguard|litevpn)
      CLI_RUN_MODES="${CLI_RUN_MODES:+$CLI_RUN_MODES }$1"
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -n "$CLI_RUN_MODES" ]]; then
  RUN_MODES="$CLI_RUN_MODES"
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

remote() {
  ssh -i "$KEY" "$HOST" "$@"
}

start_sudo_keepalive() {
  sudo -v
  while true; do
    sudo -n true >/dev/null 2>&1 || exit 0
    sleep 30
  done &
  SUDO_KEEPALIVE_PID=$!
}

stop_sudo_keepalive() {
  if [[ -n "$SUDO_KEEPALIVE_PID" ]]; then
    kill "$SUDO_KEEPALIVE_PID" >/dev/null 2>&1 || true
    wait "$SUDO_KEEPALIVE_PID" >/dev/null 2>&1 || true
    SUDO_KEEPALIVE_PID=""
  fi
}

restore_remote_litevpn() {
  remote "sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo pkill iperf3 >/dev/null 2>&1 || true; sudo systemctl start litevpn-server" || true
}

stop_local_wireguard() {
  if [[ "$LOCAL_WG_UP" == "1" ]]; then
    sudo "$WG_QUICK_BIN" down "$WG_CONF" >/dev/null 2>&1 || true
    LOCAL_WG_UP=0
  fi
}

stop_local_litevpn() {
  if [[ -n "$LITEVPN_PID" ]]; then
    kill "$LITEVPN_PID" >/dev/null 2>&1 || true
    wait "$LITEVPN_PID" >/dev/null 2>&1 || true
    LITEVPN_PID=""
  fi
  sudo pkill -f litevpn-client >/dev/null 2>&1 || true
  sudo "$ROOT/target/release/litevpn-client" --config "$LITEVPN_CONFIG" --cleanup-routes >/dev/null 2>&1 || true
}

cleanup() {
  stop_local_wireguard
  stop_local_litevpn
  restore_remote_litevpn
  stop_sudo_keepalive
}

run_bench() {
  local mode="$1"
  local log="$COMPARE_DIR/$mode.log"
  local mode_dir="$COMPARE_DIR/$mode"
  local mode_summary="$mode_dir/summary.csv"

  echo
  echo "== benchmark $mode =="
  mkdir -p "$mode_dir"
  MODE="$mode" \
    HOST="$HOST" \
    KEY="$KEY" \
    DURATION="$DURATION" \
    PARALLEL="$PARALLEL" \
    LOG_DIR="$mode_dir" \
    "$ROOT/scripts/bench-vpn-throughput.sh" 2>&1 | tee "$log"

  if [[ -f "$mode_summary" ]]; then
    tail -n +2 "$mode_summary" >> "$SUMMARY"
  fi
}

fastcom_pause() {
  local mode="$1"
  local mode_dir="$COMPARE_DIR/$mode"
  local notes="$mode_dir/fastcom.md"
  local ignored=""

  if [[ "$FASTCOM_PAUSE" != "1" ]]; then
    return
  fi
  if [[ ! -t 0 ]]; then
    echo "FASTCOM_PAUSE=1 requires an interactive terminal." >&2
    return 1
  fi

  mkdir -p "$mode_dir"
  cat > "$notes" <<EOF
# Fast.com manual result: $mode

Mode: $mode
URL: $FASTCOM_URL
Time: $(date '+%Y-%m-%dT%H:%M:%S%z')

Fill after the browser run:

- download_mbps:
- upload_mbps:
- unloaded_latency_ms:
- loaded_latency_ms:
- fastcom_client:
- fastcom_server:
- notes:
EOF

  echo
  echo "== manual Fast.com check: $mode =="
  echo "The $mode tunnel is still up."
  echo "Record Fast.com values in: $notes"
  if [[ "$OPEN_FASTCOM" == "1" ]] && command -v open >/dev/null 2>&1; then
    open "$FASTCOM_URL" >/dev/null 2>&1 || true
  fi
  read -r -p "Run Fast.com for $mode, then press Enter to continue: " ignored
}

run_wireguard() {
  if [[ ! -f "$WG_CONF" ]]; then
    echo "missing $WG_CONF; run scripts/setup-wireguard-baseline.sh first" >&2
    exit 1
  fi

  echo
  echo "== start wireguard =="
  remote "sudo systemctl stop litevpn-server; sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo wg-quick up '$WG_NAME'; sudo wg show '$WG_NAME'"
  sudo "$WG_QUICK_BIN" up "$WG_CONF"
  LOCAL_WG_UP=1
  sleep 2
  run_bench wireguard
  fastcom_pause wireguard
  stop_local_wireguard
  restore_remote_litevpn
}

run_litevpn() {
  echo
  echo "== start litevpn =="
  restore_remote_litevpn
  sudo "$ROOT/target/release/litevpn-client" \
    --config "$LITEVPN_CONFIG" \
    --connect-retries "$LITEVPN_CONNECT_RETRIES" \
    --connect-retry-delay-ms "$LITEVPN_CONNECT_RETRY_DELAY_MS" \
    > "$COMPARE_DIR/litevpn-client.log" 2>&1 &
  LITEVPN_PID=$!
  sleep 4
  run_bench litevpn
  fastcom_pause litevpn
  stop_local_litevpn
}

validate_run_modes() {
  local mode=""

  for mode in $RUN_MODES; do
    case "$mode" in
      wireguard|litevpn) ;;
      *)
        echo "unknown mode in RUN_MODES: $mode" >&2
        exit 1
        ;;
    esac
  done
}

check_local_cmd() {
  if command -v "$1" >/dev/null 2>&1; then
    echo "local_cmd:$1=$(command -v "$1")"
  else
    echo "missing local command: $1" >&2
    return 1
  fi
}

check_local_file() {
  if [[ -f "$1" ]]; then
    echo "local_file:$1=ok"
  else
    echo "missing local file: $1" >&2
    return 1
  fi
}

preflight() {
  local ok=1
  local endpoint=""

  echo "== VPN comparison preflight =="
  echo "host=$HOST"
  echo "wg_conf=$WG_CONF"
  echo "litevpn_config=$LITEVPN_CONFIG"
  echo "note=WireGuard uses UDP 443 by stopping remote litevpn-server before wg0 starts."

  for cmd in ssh sudo wg wg-quick wireguard-go iperf3 jq ping; do
    check_local_cmd "$cmd" || ok=0
  done

  if [[ -z "$WG_QUICK_BIN" ]] && command -v wg-quick >/dev/null 2>&1; then
    WG_QUICK_BIN="$(command -v wg-quick)"
  fi
  if [[ -n "$WG_QUICK_BIN" ]]; then
    if [[ -x "$WG_QUICK_BIN" ]]; then
      echo "wg_quick_bin=$WG_QUICK_BIN"
    else
      echo "wg_quick_bin_not_executable=$WG_QUICK_BIN" >&2
      ok=0
    fi
  fi

  check_local_file "$WG_CONF" || ok=0
  check_local_file "$LITEVPN_CONFIG" || ok=0
  check_local_file "$ROOT/target/release/litevpn-client" || ok=0

  if [[ -f "$WG_CONF" ]]; then
    endpoint="$(awk -F' = ' '$1 == "Endpoint" { print $2; exit }' "$WG_CONF")"
    [[ -n "$endpoint" ]] && echo "wireguard_endpoint=$endpoint"
  fi

  if [[ -d "$ROOT/.git" ]] && command -v git >/dev/null 2>&1; then
    if git -C "$ROOT" check-ignore -q "$WG_CONF"; then
      echo "git_ignore:$WG_CONF=ok"
    else
      echo "git_ignore:$WG_CONF=missing" >&2
      ok=0
    fi
  fi

  echo
  echo "== remote checks =="
  if remote "sudo -n true && sudo -n test -f '/etc/wireguard/$WG_NAME.conf' && command -v wg && command -v wg-quick && command -v iperf3 && systemctl is-active litevpn-server"; then
    echo "remote_preflight=ok"
  else
    echo "remote_preflight=failed" >&2
    ok=0
  fi

  if [[ "$ok" == "1" ]]; then
    echo "preflight_ok=1"
  else
    echo "preflight_ok=0"
    return 1
  fi
}

if [[ "$PREFLIGHT" == "1" ]]; then
  preflight
  exit $?
fi

need ssh
need sudo
need wg-quick
need iperf3
need jq
need ping

if [[ -z "$WG_QUICK_BIN" ]]; then
  WG_QUICK_BIN="$(command -v wg-quick)"
fi
validate_run_modes

mkdir -p "$COMPARE_DIR"
echo "mode,ping_min_ms,ping_avg_ms,ping_max_ms,ping_stddev_ms,upload_sender_mbps,upload_receiver_mbps,download_sender_mbps,download_receiver_mbps,log_dir" > "$SUMMARY"

echo "== VPN mode comparison =="
date '+%Y-%m-%dT%H:%M:%S%z'
echo "host=$HOST"
echo "duration_secs=$DURATION parallel=$PARALLEL"
echo "run_modes=$RUN_MODES"
echo "fastcom_pause=$FASTCOM_PAUSE"
echo "logs=$COMPARE_DIR"

start_sudo_keepalive
trap cleanup EXIT INT TERM

for mode in $RUN_MODES; do
  case "$mode" in
    wireguard) run_wireguard ;;
    litevpn) run_litevpn ;;
    *)
      echo "unknown mode in RUN_MODES: $mode" >&2
      exit 1
      ;;
  esac
done

echo
echo "comparison logs: $COMPARE_DIR"
echo "comparison summary: $SUMMARY"
echo "comparison report: $ROOT/scripts/summarize-vpn-comparison.sh \"$COMPARE_DIR\""
cat "$SUMMARY"
