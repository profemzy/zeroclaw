#!/usr/bin/env bash
# oluto-receipt.sh — Create expense from LLM-extracted receipt data + attach image
# Usage: oluto-receipt.sh FILE_PATH VENDOR AMOUNT DATE [CATEGORY] [GST] [PST]
# Output: Human-readable summary (NOT raw JSON)
#
# The LLM extracts receipt data via vision analysis and passes pre-validated
# fields to this script. This script handles: transaction matching, expense
# creation, and receipt image attachment.
#
# NOTE: This runs on Alpine/BusyBox — do NOT use grep -P (Perl regex).
set -euo pipefail

export PATH="$HOME/.local/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
API="$SCRIPT_DIR/oluto-api.sh"
AUTH="$SCRIPT_DIR/oluto-auth.sh"
MATCH="$SCRIPT_DIR/oluto-match-transaction.sh"
ATTACH="$SCRIPT_DIR/oluto-attach-receipt.sh"
CONFIG_FILE="$HOME/.oluto-config.json"

if [ $# -lt 4 ]; then
    echo "Usage: oluto-receipt.sh FILE_PATH VENDOR AMOUNT DATE [CATEGORY] [GST] [PST]"
    echo "Error: Please provide at least FILE_PATH, VENDOR, AMOUNT, and DATE."
    exit 1
fi

FILE_PATH="$1"
VENDOR="$2"
AMOUNT="$3"
DATE="$4"
CATEGORY="${5:-Meals and Entertainment}"
GST_AMOUNT="${6:-0.00}"
PST_AMOUNT="${7:-0.00}"

if [ ! -f "$FILE_PATH" ]; then
    echo "Error: File not found at $FILE_PATH"
    exit 1
fi

# Validate amount is numeric
if ! echo "$AMOUNT" | grep -qE '^[0-9]+(\.[0-9]+)?$'; then
    echo "Error: Invalid amount '$AMOUNT'. Expected a positive number."
    exit 1
fi

BID="${OLUTO_BUSINESS_ID:-$(jq -r '.default_business_id' "$CONFIG_FILE")}"
TOKEN=$("$AUTH")

# Step 1: Try to match existing transaction
MATCHES="[]"
if [ -n "$DATE" ]; then
    MATCHES=$("$MATCH" "$AMOUNT" "$DATE" "$VENDOR" 2>/dev/null || echo '[]')
fi

MATCH_COUNT=$(echo "$MATCHES" | jq 'length' 2>/dev/null || echo 0)

# Step 2: Handle result
if [ "$MATCH_COUNT" -gt 0 ]; then
    # Found matching transaction — attach receipt to it
    MATCH_VENDOR=$(echo "$MATCHES" | jq -r '.[0].vendor_name // "Unknown"')
    MATCH_AMOUNT=$(echo "$MATCHES" | jq -r '.[0].amount // "0.00"')
    MATCH_DATE=$(echo "$MATCHES" | jq -r '.[0].transaction_date // "unknown"')
    MATCH_ID=$(echo "$MATCHES" | jq -r '.[0].id // ""')

    # Attach receipt image to the matched transaction
    if [ -n "$MATCH_ID" ]; then
        "$ATTACH" "$MATCH_ID" "$FILE_PATH" 2>/dev/null || true
    fi

    echo "Receipt matched existing transaction: \$$MATCH_AMOUNT at $MATCH_VENDOR on $MATCH_DATE (ID: $MATCH_ID)"
else
    # No match — create new expense
    TXN_DATE="${DATE:-$(date +%Y-%m-%d)}"
    DESCRIPTION="Receipt capture - $VENDOR"

    # Negate amount for expense (LedgerForge stores expenses as negative)
    NEG_AMOUNT="-${AMOUNT}"

    BODY=$(jq -n \
        --arg vendor "$VENDOR" \
        --arg amount "$NEG_AMOUNT" \
        --arg date "$TXN_DATE" \
        --arg desc "$DESCRIPTION" \
        --arg category "$CATEGORY" \
        --arg gst "$GST_AMOUNT" \
        --arg pst "$PST_AMOUNT" \
        '{
            vendor_name: $vendor,
            amount: $amount,
            transaction_date: $date,
            description: $desc,
            classification: "business_expense",
            category: $category,
            currency: "CAD",
            gst_amount: $gst,
            pst_amount: $pst
        }')

    RESULT=$("$API" POST "/api/v1/businesses/$BID/transactions" "$BODY" 2>&1 || true)

    # Check if creation succeeded
    TXN_ID=$(echo "$RESULT" | jq -r '.id // empty' 2>/dev/null || true)

    if [ -n "$TXN_ID" ]; then
        # Attach receipt image to the new transaction
        "$ATTACH" "$TXN_ID" "$FILE_PATH" 2>/dev/null || true

        TAX_INFO=""
        [ "$GST_AMOUNT" != "0.00" ] && TAX_INFO=" | GST: \$$GST_AMOUNT"
        [ "$PST_AMOUNT" != "0.00" ] && TAX_INFO="$TAX_INFO | PST: \$$PST_AMOUNT"
        echo "Receipt processed: \$$AMOUNT at $VENDOR on $TXN_DATE (ID: $TXN_ID)"
        echo "Category: ${CATEGORY}${TAX_INFO}"
        echo "Saved as draft expense. Receipt image stored."
    else
        echo "Receipt from $VENDOR: \$$AMOUNT on $TXN_DATE"
        echo "Could not save to ledger. Please create manually."
    fi
fi
