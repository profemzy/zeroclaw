#!/usr/bin/env bash
# Test script to verify RBAC enforcement in oluto-api.sh
# Run: ./scripts/test_oluto_rbac.sh
#
# Tests that viewers are blocked from write operations (POST/PATCH/PUT/DELETE)
# and that non-viewers (admin, accountant, unset) are not blocked by the RBAC guard.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
API_SCRIPT="$PROJECT_ROOT/skills/oluto/scripts/oluto-api.sh"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m'

PASS=0
FAIL=0

log_pass() {
    echo -e "${GREEN}✓${NC} $1"
    PASS=$((PASS + 1))
}

log_fail() {
    echo -e "${RED}✗${NC} $1"
    FAIL=$((FAIL + 1))
}

echo "=== Testing RBAC enforcement in oluto-api.sh ==="

# ── Test 1: viewer + POST → blocked with RBAC error ──
OUTPUT=$(OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" POST /api/v1/test '{}' 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_pass "viewer + POST is blocked with 'Access denied'"
else
    log_fail "viewer + POST was NOT blocked (output: $OUTPUT)"
fi

# Verify exit code
if ! OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" POST /api/v1/test '{}' 2>/dev/null; then
    log_pass "viewer + POST exits with non-zero code"
else
    log_fail "viewer + POST exited with zero code"
fi

# ── Test 2: viewer + PATCH → blocked ──
OUTPUT=$(OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" PATCH /api/v1/test '{}' 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_pass "viewer + PATCH is blocked"
else
    log_fail "viewer + PATCH was NOT blocked"
fi

# ── Test 3: viewer + PUT → blocked ──
OUTPUT=$(OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" PUT /api/v1/test '{}' 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_pass "viewer + PUT is blocked"
else
    log_fail "viewer + PUT was NOT blocked"
fi

# ── Test 4: viewer + DELETE → blocked ──
OUTPUT=$(OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" DELETE /api/v1/test 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_pass "viewer + DELETE is blocked"
else
    log_fail "viewer + DELETE was NOT blocked"
fi

# ── Test 5: viewer + GET → NOT blocked by RBAC ──
# (will fail downstream at config/auth, but should NOT show "Access denied")
OUTPUT=$(OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" GET /api/v1/test 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_fail "viewer + GET was incorrectly blocked by RBAC"
else
    log_pass "viewer + GET passes RBAC check (fails later at auth, as expected)"
fi

# ── Test 6: admin + POST → NOT blocked by RBAC ──
OUTPUT=$(OLUTO_USER_ROLE=admin bash "$API_SCRIPT" POST /api/v1/test '{}' 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_fail "admin + POST was incorrectly blocked by RBAC"
else
    log_pass "admin + POST passes RBAC check"
fi

# ── Test 7: accountant + POST → NOT blocked by RBAC ──
OUTPUT=$(OLUTO_USER_ROLE=accountant bash "$API_SCRIPT" POST /api/v1/test '{}' 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_fail "accountant + POST was incorrectly blocked by RBAC"
else
    log_pass "accountant + POST passes RBAC check"
fi

# ── Test 8: no role set + POST → NOT blocked by RBAC ──
OUTPUT=$(unset OLUTO_USER_ROLE; bash "$API_SCRIPT" POST /api/v1/test '{}' 2>&1 || true)
if echo "$OUTPUT" | grep -q "Access denied"; then
    log_fail "unset role + POST was incorrectly blocked by RBAC"
else
    log_pass "unset role + POST passes RBAC check"
fi

# ── Test 9: error message includes the method name ──
OUTPUT=$(OLUTO_USER_ROLE=viewer bash "$API_SCRIPT" DELETE /api/v1/test 2>&1 || true)
if echo "$OUTPUT" | grep -q "DELETE operations"; then
    log_pass "error message includes the HTTP method (DELETE)"
else
    log_fail "error message does not include the HTTP method"
fi

# Summary
echo ""
echo "=== Summary ==="
echo -e "Passed: ${GREEN}$PASS${NC}"
echo -e "Failed: ${RED}$FAIL${NC}"

if [[ $FAIL -gt 0 ]]; then
    echo -e "${RED}FAILED${NC}: $FAIL tests failed"
    exit 1
else
    echo -e "${GREEN}PASSED${NC}: All tests passed"
    exit 0
fi
