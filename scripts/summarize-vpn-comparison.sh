#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULT_DIR="${1:-}"
WRITE_REPORT="${WRITE_REPORT:-1}"

usage() {
  cat <<'HELP'
Usage:
  scripts/summarize-vpn-comparison.sh
  scripts/summarize-vpn-comparison.sh bench-results/vpn-compare-YYYYMMDD-HHMMSS

Reads a WireGuard/LiteVPN comparison summary.csv and optional per-mode
fastcom.md notes, prints a Markdown report, and writes comparison.md beside the
logs by default.

Environment:
  WRITE_REPORT=1
HELP
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "$RESULT_DIR" && -d "$ROOT/bench-results" ]]; then
  RESULT_DIR="$(find "$ROOT/bench-results" -maxdepth 1 -type d -name 'vpn-compare-*' 2>/dev/null | sort | tail -1)"
fi

if [[ -z "$RESULT_DIR" ]]; then
  echo "no vpn-compare result directory found under bench-results/" >&2
  exit 1
fi

if [[ "$RESULT_DIR" != /* ]]; then
  RESULT_DIR="$ROOT/$RESULT_DIR"
fi

SUMMARY="$RESULT_DIR/summary.csv"
REPORT="$RESULT_DIR/comparison.md"

if [[ ! -f "$SUMMARY" ]]; then
  echo "missing summary.csv: $SUMMARY" >&2
  exit 1
fi

extract_fastcom_value() {
  local file="$1"
  local key="$2"

  if [[ ! -f "$file" ]]; then
    return 0
  fi

  awk -F': *' -v key="$key" '
    $1 ~ "^- " key "$" {
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2)
      print $2
      exit
    }
  ' "$file"
}

emit_report() {
  local generated_at=""
  local metric_rows=""
  local scored_rows=""
  local verdict=""
  local mode=""
  local notes=""
  local fast_file=""
  local fast_download=""
  local fast_upload=""
  local fast_unloaded=""
  local fast_loaded=""
  local fast_client=""
  local fast_server=""

  generated_at="$(date '+%Y-%m-%dT%H:%M:%S%z')"

  echo "# VPN Comparison Report"
  echo
  echo "- result_dir: \`$RESULT_DIR\`"
  echo "- generated_at: \`$generated_at\`"
  echo "- score: \`download_receiver_mbps + upload_receiver_mbps - ping_avg_ms / 10 - fast_loaded_latency_ms / 20\`"
  echo "- note: Fast.com latency is optional; empty values are ignored in the score."
  echo
  echo "## Tunnel Throughput"
  echo
  echo "| mode | ping avg ms | upload recv Mbps | download recv Mbps | score |"
  echo "| --- | ---: | ---: | ---: | ---: |"

  metric_rows="$(
    awk -F',' '
      NR == 1 { next }
      NF >= 9 {
        mode=$1
        printf "%s,%s,%s,%s\n", mode, $3, $7, $9
      }
    ' "$SUMMARY"
  )"

  if [[ -z "$metric_rows" ]]; then
    echo "| pending |  |  |  |  |"
  else
    while IFS=',' read -r mode ping_avg upload_recv download_recv; do
      fast_file="$RESULT_DIR/$mode/fastcom.md"
      fast_loaded="$(extract_fastcom_value "$fast_file" "loaded_latency_ms")"
      score="$(
        awk -v download="$download_recv" -v upload="$upload_recv" -v ping="$ping_avg" -v loaded="$fast_loaded" '
          BEGIN {
            score = download + upload - (ping / 10)
            if (loaded ~ /^[0-9]+([.][0-9]+)?$/) {
              score -= loaded / 20
            }
            printf "%.2f", score
          }
        '
      )"
      scored_rows="${scored_rows}${scored_rows:+$'\n'}$mode,$ping_avg,$upload_recv,$download_recv,$score"
      echo "| $mode | $ping_avg | $upload_recv | $download_recv | $score |"
    done <<< "$metric_rows"
  fi

  echo
  echo "## Fast.com"
  echo
  echo "| mode | download Mbps | upload Mbps | unloaded latency ms | loaded latency ms | client | server |"
  echo "| --- | ---: | ---: | ---: | ---: | --- | --- |"

  while IFS=',' read -r mode _ping_avg _upload_recv _download_recv _score; do
    [[ -z "$mode" ]] && continue
    fast_file="$RESULT_DIR/$mode/fastcom.md"
    fast_download="$(extract_fastcom_value "$fast_file" "download_mbps")"
    fast_upload="$(extract_fastcom_value "$fast_file" "upload_mbps")"
    fast_unloaded="$(extract_fastcom_value "$fast_file" "unloaded_latency_ms")"
    fast_loaded="$(extract_fastcom_value "$fast_file" "loaded_latency_ms")"
    fast_client="$(extract_fastcom_value "$fast_file" "fastcom_client")"
    fast_server="$(extract_fastcom_value "$fast_file" "fastcom_server")"
    echo "| $mode | ${fast_download:-} | ${fast_upload:-} | ${fast_unloaded:-} | ${fast_loaded:-} | ${fast_client:-} | ${fast_server:-} |"
  done <<< "$scored_rows"

  if [[ -z "$scored_rows" ]]; then
    echo "| pending |  |  |  |  |  |  |"
  fi

  echo
  echo "## Selection"
  echo
  verdict="$(
    awk -F',' '
      NF >= 5 {
        mode=$1
        ping=$2 + 0
        upload=$3 + 0
        download=$4 + 0
        score=$5 + 0
        if (!seen || score > best_score) {
          seen=1
          best_mode=mode
          best_score=score
          best_download=download
          best_upload=upload
          best_ping=ping
        }
      }
      END {
        if (seen) {
          printf "recommended_mode=%s\nscore=%.2f\ndownload_receiver_mbps=%.2f\nupload_receiver_mbps=%.2f\nping_avg_ms=%.3f\n", best_mode, best_score, best_download, best_upload, best_ping
        } else {
          print "recommended_mode=pending"
        }
      }
    ' <<< "$scored_rows"
  )"
  while IFS= read -r line; do
    echo "- $line"
  done <<< "$verdict"

  echo
  echo "## Raw Summary"
  echo
  echo '```csv'
  cat "$SUMMARY"
  echo '```'

  notes=""
  for fast_file in "$RESULT_DIR"/*/fastcom.md; do
    [[ -f "$fast_file" ]] || continue
    mode="$(basename "$(dirname "$fast_file")")"
    notes="${notes}${notes:+$'\n'}- $mode: \`$fast_file\`"
  done
  if [[ -n "$notes" ]]; then
    echo
    echo "## Fast.com Note Files"
    echo
    printf '%s\n' "$notes"
  fi
}

if [[ "$WRITE_REPORT" == "1" ]]; then
  emit_report | tee "$REPORT"
else
  emit_report
fi
