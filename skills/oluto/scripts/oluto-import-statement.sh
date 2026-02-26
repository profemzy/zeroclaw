#!/usr/bin/env bash
# oluto-import-statement.sh — Parse AND import a bank statement (CSV or PDF).
# Usage: oluto-import-statement.sh <file_path>
#
# This script does everything in one shot:
#   1. Uploads the file to the parse endpoint
#   2. Polls for results (PDF only) with progressive status updates
#   3. Confirms/imports the parsed transactions as drafts
#   4. Cleans up the local file on success
#
# Output: Human-readable summary (NOT raw JSON)
#
# NOTE: This runs on Alpine/BusyBox — do NOT use grep -P (Perl regex).
set -euo pipefail

export PATH="$HOME/.local/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_FILE="${HOME}/.oluto-config.json"

if [ $# -lt 1 ]; then
    echo "Usage: oluto-import-statement.sh <file_path>" >&2
    exit 1
fi

FILE_PATH="$1"

if [ ! -f "$FILE_PATH" ]; then
    echo "ERROR: File not found: $FILE_PATH" >&2
    exit 1
fi

# Get auth token
TOKEN=$("${SCRIPT_DIR}/oluto-auth.sh")
BASE_URL=$(jq -r '.base_url' "$CONFIG_FILE")

# Determine business_id
BID="${OLUTO_BUSINESS_ID:-}"
if [ -z "$BID" ]; then
    BID=$(jq -r '.default_business_id // empty' "$CONFIG_FILE" 2>/dev/null || true)
fi
if [ -z "$BID" ]; then
    echo "ERROR: No business_id available. Set OLUTO_BUSINESS_ID or default_business_id in config." >&2
    exit 1
fi

# Detect file type
FILENAME=$(basename "$FILE_PATH")
EXT="${FILENAME##*.}"
EXT_LOWER=$(echo "$EXT" | tr '[:upper:]' '[:lower:]')

if [ "$EXT_LOWER" != "csv" ] && [ "$EXT_LOWER" != "pdf" ]; then
    echo "ERROR: Unsupported file type '.$EXT_LOWER'. Only CSV and PDF are supported." >&2
    exit 1
fi

# Helper: show CSV header on failure for column mapping hints
show_csv_header() {
    if [ "$EXT_LOWER" = "csv" ] && [ -f "$FILE_PATH" ]; then
        HEADER_LINE=$(head -1 "$FILE_PATH" 2>/dev/null || true)
        if [ -n "$HEADER_LINE" ]; then
            echo ""
            echo "CSV_HEADER: $HEADER_LINE"
            echo "The CSV parser expects columns for: date, description/memo, and amount (or separate debit/credit columns)."
            echo "Common supported formats: RBC, TD, BMO, Scotiabank, CIBC, Desjardins CSV exports."
        fi
    fi
}

# ── Step 1: Parse ──
PARSE_URL="${BASE_URL}/api/v1/businesses/${BID}/transactions/import/parse"

RESPONSE=$(curl -s -w "\n%{http_code}" -X POST "$PARSE_URL" \
    -H "Authorization: Bearer ${TOKEN}" \
    -F "file=@\"${FILE_PATH}\"" 2>&1)

HTTP_CODE=$(echo "$RESPONSE" | tail -1)
BODY=$(echo "$RESPONSE" | sed '$d')

if [ "${HTTP_CODE:0:1}" != "2" ]; then
    echo "ERROR (HTTP ${HTTP_CODE}): Failed to parse statement."
    if echo "$BODY" | jq . >/dev/null 2>&1; then
        echo "$BODY" | jq -r '.error // .message // .detail // "Unknown error"'
    else
        echo "$BODY"
    fi
    # Contextual guidance based on HTTP code
    case "$HTTP_CODE" in
        400) echo "Tip: The file format may not be recognized. Ensure it is a valid CSV or PDF bank statement." ;;
        413) echo "Tip: The file is too large. Maximum file size is 20MB." ;;
        422) echo "Tip: The file was read but could not be parsed. Check that it contains transaction data with dates, descriptions, and amounts." ;;
        401|403) echo "Tip: Authentication failed. Please re-authenticate." ;;
    esac
    show_csv_header
    exit 1
fi

SUCCESS=$(echo "$BODY" | jq -r '.success // false')
if [ "$SUCCESS" != "true" ]; then
    MSG=$(echo "$BODY" | jq -r '.error // "Parse failed"')
    echo "ERROR: $MSG" >&2
    show_csv_header
    exit 1
fi

DATA=$(echo "$BODY" | jq '.data')

# Check if this is an async job response (has job_id)
JOB_ID=$(echo "$DATA" | jq -r '.job_id // empty' 2>/dev/null || true)

PARSE_RESULT=""

if [ -n "$JOB_ID" ]; then
    # PDF async processing — poll until complete
    echo "Processing PDF statement (job: $JOB_ID)..." >&2
    MAX_POLLS=80  # 4 minutes max (80 * 3s = 240s, fits within 300s shell timeout)
    POLL_INTERVAL=3

    for i in $(seq 1 $MAX_POLLS); do
        sleep $POLL_INTERVAL

        JOB_URL="${BASE_URL}/api/v1/businesses/${BID}/transactions/jobs/${JOB_ID}"
        JOB_RESP=$(curl -s -X GET "$JOB_URL" \
            -H "Authorization: Bearer ${TOKEN}" \
            -H "Content-Type: application/json")

        JOB_SUCCESS=$(echo "$JOB_RESP" | jq -r '.success // false')
        if [ "$JOB_SUCCESS" != "true" ]; then
            continue
        fi

        JOB_STATUS=$(echo "$JOB_RESP" | jq -r '.data.status // "pending"')

        if [ "$JOB_STATUS" = "completed" ]; then
            PARSE_RESULT=$(echo "$JOB_RESP" | jq -c '.data.result_data')
            break
        elif [ "$JOB_STATUS" = "failed" ]; then
            ERROR=$(echo "$JOB_RESP" | jq -r '.data.error // "Processing failed"')
            echo "ERROR: PDF processing failed: $ERROR" >&2
            # Surface parse warnings from the failed job
            JOB_WARNINGS=$(echo "$JOB_RESP" | jq -r '.data.parse_warnings // [] | join("; ")' 2>/dev/null || true)
            if [ -n "$JOB_WARNINGS" ]; then
                echo "Parse warnings: $JOB_WARNINGS" >&2
            fi
            echo "Tip: If this is a scanned PDF, try re-scanning at higher resolution. If it is a secured/encrypted PDF, try exporting as CSV from your bank's website." >&2
            exit 1
        fi

        # Progress update every 30 seconds (every 10 polls)
        if [ $((i % 10)) -eq 0 ]; then
            ELAPSED=$((i * POLL_INTERVAL))
            echo "Still processing PDF... (${ELAPSED}s elapsed)" >&2
        fi
    done

    if [ -z "$PARSE_RESULT" ]; then
        echo "ERROR: PDF processing timed out after $((MAX_POLLS * POLL_INTERVAL)) seconds." >&2
        echo "Tip: The PDF may be too large or complex. Try splitting into smaller files, or export as CSV from your bank's website for faster processing." >&2
        exit 1
    fi
else
    # CSV immediate response
    PARSE_RESULT=$(echo "$DATA" | jq -c '.')
fi

# ── Show parse summary ──
TOTAL=$(echo "$PARSE_RESULT" | jq -r '.total_count // 0')
DUPES=$(echo "$PARSE_RESULT" | jq -r '.duplicate_count // 0')
PERIOD=$(echo "$PARSE_RESULT" | jq -r '.statement_period // "unknown"')
ACCOUNT=$(echo "$PARSE_RESULT" | jq -r '.account_info // "unknown"')
WARNINGS=$(echo "$PARSE_RESULT" | jq -r '.parse_warnings | length')

echo "Parsed: $TOTAL transactions, $DUPES duplicates"
echo "Account: $ACCOUNT"
echo "Period: $PERIOD"
if [ "$WARNINGS" -gt 0 ]; then
    echo "Warnings: $(echo "$PARSE_RESULT" | jq -r '.parse_warnings | join("; ")')"
fi

if [ "$TOTAL" -eq 0 ]; then
    echo "No transactions found in the statement."
    if [ "$DUPES" -gt 0 ]; then
        echo "Note: $DUPES duplicate transactions were detected — these may have been imported previously."
    fi
    if [ "$EXT_LOWER" = "csv" ] && [ -f "$FILE_PATH" ]; then
        LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null | tr -d ' ' || echo "0")
        HEADER_LINE=$(head -1 "$FILE_PATH" 2>/dev/null || true)
        echo "CSV has $LINE_COUNT lines. Header: $HEADER_LINE"
    fi
    echo "Tip: Verify the file contains transaction data. For CSV files, ensure columns include date, description, and amount."
    # Clean up — file is no longer useful
    rm -f "$FILE_PATH" 2>/dev/null || true
    exit 0
fi

# ── Step 2: Confirm import ──
CONFIRM_URL="${BASE_URL}/api/v1/businesses/${BID}/transactions/import/confirm"

# Save parse result to temp file to avoid shell argument length issues
TMPFILE=$(mktemp /tmp/oluto-import-XXXXXX.json)
echo "$PARSE_RESULT" > "$TMPFILE"

RESPONSE=$(curl -s -w "\n%{http_code}" -X POST "$CONFIRM_URL" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "Content-Type: application/json" \
    -d @"$TMPFILE")

rm -f "$TMPFILE"

HTTP_CODE=$(echo "$RESPONSE" | tail -1)
BODY=$(echo "$RESPONSE" | sed '$d')

if [ "${HTTP_CODE:0:1}" != "2" ]; then
    echo "ERROR (HTTP ${HTTP_CODE}): Failed to confirm import."
    if echo "$BODY" | jq . >/dev/null 2>&1; then
        echo "$BODY" | jq -r '.error // .message // .detail // "Unknown error"'
    else
        echo "$BODY"
    fi
    case "$HTTP_CODE" in
        409) echo "Tip: Some transactions may have already been imported. Check for duplicates in your ledger." ;;
        422) echo "Tip: The parsed transactions could not be validated. Try re-uploading the original file." ;;
    esac
    exit 1
fi

CONFIRM_SUCCESS=$(echo "$BODY" | jq -r '.success // false')
if [ "$CONFIRM_SUCCESS" != "true" ]; then
    MSG=$(echo "$BODY" | jq -r '.error // "Confirm failed"')
    echo "ERROR: $MSG" >&2
    exit 1
fi

CONFIRM_DATA=$(echo "$BODY" | jq '.data')
BATCH_ID=$(echo "$CONFIRM_DATA" | jq -r '.batch_id // empty' 2>/dev/null || true)
IMPORTED_COUNT=$(echo "$CONFIRM_DATA" | jq -r '.imported_count // .count // .transactions_count // 0' 2>/dev/null || echo "0")

echo "Imported $IMPORTED_COUNT transactions as drafts."
echo ""
echo "IMPORT COMPLETE: $IMPORTED_COUNT draft transactions from $ACCOUNT ($PERIOD)"
echo "Review and post them from the Transactions page on the Dashboard."

# Clean up uploaded statement file (data is now in LedgerForge)
if [ -f "$FILE_PATH" ]; then
    rm -f "$FILE_PATH"
fi
