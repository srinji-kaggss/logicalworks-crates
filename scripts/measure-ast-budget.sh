#!/bin/sh
# Measure lgwks_ast's per-grammar parse budget (#277).
#
# One process per (grammar, shape), because peak resident set size is a
# high-water mark for a whole process: a run that walked every grammar and every
# shape could only report one number, and that number would be the widest tree
# any of them produced rather than any grammar's own cost. One process each also
# keeps one crashing grammar from destroying the measurements around it, which
# is not hypothetical: the markdown grammar aborts the process on a nested list
# (see the `hostile.rs` module), and a rig that stopped there would have reported
# seventeen grammars instead of twenty-eight.
#
# The example prints its own timings; this script supplies the one thing a
# process cannot measure about itself after the fact, the peak, and writes both
# to one TSV per run so a README table can be regenerated rather than retyped.
# A run that dies is recorded with its exit status and its signal, because a
# missing row is indistinguishable from a shape that was never measured.
#
# Usage:
#   scripts/measure-ast-budget.sh [output-directory]
#
# Defaults to a directory under the system temp root, and prints its path. The
# per-run files are the evidence; the directory is left for the caller to
# delete, because a measurement rig that deleted its own output would discard
# the record it exists to produce.
#
# Environment overrides, all with the issue's numbers as their defaults:
#   AST_BUDGET_BYTES       2097152   bytes per representative and long-line source
#   AST_BUDGET_SHAPE_BYTES 2097152   bytes per adversarial source
#   AST_BUDGET_ROUNDS      3         samples per measurement
#   AST_BUDGET_TIERS       "100 1000 10000 100000"
#   AST_BUDGET_TIER_BYTES  65536     bytes per parse inside a tier
#   AST_BUDGET_THREADS     8         workers a tier may admit
#   AST_BUDGET_TIMEOUT     180       seconds one run may take before it is recorded as unfinished

set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
out=${1:-${TMPDIR:-/tmp}/lgwks-ast-budget-$$}
example="$root/target/release/examples/parse_budget"

if [ ! -x "$example" ]; then
    echo "building the release example into $root/target" >&2
    ( cd "$root" && cargo build --locked --release -p lgwks_ast --features full --example parse_budget )
fi

mkdir -p "$out"

# The grammar and shape tables are read from the example itself rather than
# restated here: a second list is a second thing that can fall behind the first.
grammars=$("$example" --list-grammars)
shapes="representative nested longline unbalanced"

bytes=${AST_BUDGET_BYTES:-2097152}
shape_bytes=${AST_BUDGET_SHAPE_BYTES:-2097152}
rounds=${AST_BUDGET_ROUNDS:-3}
tiers=${AST_BUDGET_TIERS:-"100 1000 10000 100000"}
tier_bytes=${AST_BUDGET_TIER_BYTES:-65536}
threads=${AST_BUDGET_THREADS:-8}
timeout=${AST_BUDGET_TIMEOUT:-180}

# `time -l` on macOS, `time -v` on GNU. Both print the peak; the label differs, so
# both spellings are parsed when a table is built from these files.
if [ "$(uname -s)" = "Darwin" ]; then
    time_flag=-l
else
    time_flag=-v
fi

# Run one measurement under the host timer, recording rather than propagating a
# non-zero exit: a crashed process is a result this rig exists to report.
#
# The run is also bounded. A GLR parser given deeply nested delimiters at the
# byte ceiling can take hours, and a sweep that stops at the first one reports
# fewer grammars than it claims to rather than reporting the one that does not
# finish -- which is itself the measurement (#277 item 1, a parse deadline, is
# not available: ast-grep-core 0.45 builds its parser internally and does not
# expose tree-sitter's progress callback). A run that reaches the bound is killed
# and recorded as unfinished, with the elapsed time, so the table has a row for
# it instead of a hole.
timed() {
    name=$1
    shift
    set +e
    # Job control puts the runner in its own process group, so the timeout can
    # signal the *whole* group. Signalling only `/usr/bin/time` leaves the
    # measurement itself running, and it then competes for the CPU with every
    # later run and overwrites this row's tail -- a rig that measures the
    # machine's load rather than its own subject.
    set -m
    /usr/bin/time "$time_flag" "$example" "$@" > "$out/$name.tsv" 2> "$out/$name.time" &
    runner=$!
    set +m
    waited=0
    unfinished=0
    while kill -0 "$runner" 2>/dev/null; do
        if [ "$waited" -ge "$timeout" ]; then
            kill -TERM "-$runner" 2>/dev/null
            sleep 1
            kill -KILL "-$runner" 2>/dev/null
            unfinished=1
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    wait "$runner" 2>/dev/null
    status=$?
    set -e
    if [ "$unfinished" -eq 1 ]; then
        printf 'unfinished\t%s\tafter\t%s\ts\tpartial_rows_kept\n' "$name" "$timeout" \
            >> "$out/$name.tsv"
        peak=$(sed -n -E 's/^[[:space:]]*([0-9]+)[[:space:]]*(maximum resident set size|Maximum resident set size \(kbytes\)).*/\1/p' "$out/$name.time" | tail -1)
        printf '%-28s UNFINISHED after %ss (peak_rss_bytes=%s, partial row kept)\n' \
            "$name" "$timeout" "${peak:-none}" >&2
        return 0
    fi
    peak=$(sed -n -E 's/^[[:space:]]*([0-9]+)[[:space:]]*(maximum resident set size|Maximum resident set size \(kbytes\)).*/\1/p' "$out/$name.time" | tail -1)
    if [ "$time_flag" = "-v" ] && [ -n "${peak:-}" ]; then
        # GNU reports kibibytes under a label that does not say so.
        peak=$((peak * 1024))
    fi
    printf 'run\t%s\texit\t%s\tpeak_rss_bytes\t%s\n' "$name" "$status" "${peak:-none}" >> "$out/$name.tsv"
    printf '%-28s exit=%-4s peak_rss_bytes=%s\n' "$name" "$status" "${peak:-none}" >&2
}

# The floor: the same binary, the same build, parsing nothing. A grammar's peak is
# only readable against this, because the binary carries all 28 grammar tables
# and its own text whether or not a grammar is used.
"$example" --list-grammars > "$out/00-floor.tsv" 2>&1
printf 'run\t00-floor\texit\t0\tpeak_rss_bytes\tfloor\n' >> "$out/00-floor.tsv"
if [ "$(uname -s)" = "Darwin" ]; then
    /usr/bin/time -l "$example" --list-grammars > /dev/null 2> "$out/00-floor.time"
else
    /usr/bin/time -v "$example" --list-grammars > /dev/null 2> "$out/00-floor.time"
fi
sed -n -E 's/^[[:space:]]*([0-9]+)[[:space:]]*(maximum resident set size|Maximum resident set size \(kbytes\)).*/floor_peak_rss_bytes\t\1/p' "$out/00-floor.time" >> "$out/00-floor.tsv"

index=0
for grammar in $grammars; do
    index=$((index + 1))
    for shape in $shapes; do
        timed "$(printf '%02d-%s-%s' "$index" "$grammar" "$shape")" \
            --grammar "$grammar" \
            --shape "$shape" \
            --bytes "$bytes" \
            --shape-bytes "$shape_bytes" \
            --rounds "$rounds"
    done
done

index=0
for grammar in $grammars; do
    index=$((index + 1))
    level=0
    for tier in $tiers; do
        level=$((level + 1))
        timed "$(printf '%02d-%s-tier%02d' "$index" "$grammar" "$level")" \
            --grammar "$grammar" \
            --shape representative \
            --tier "$tier" \
            --tier-bytes "$tier_bytes" \
            --threads "$threads"
    done
done

printf '\n%s run(s) written under %s\n' "$(ls "$out" | grep -c '\.tsv$')" "$out"
printf 'host: %s %s, %s cores\n' "$(uname -s)" "$(uname -r)" "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo '?')"
printf 'bytes=%s shape_bytes=%s rounds=%s tiers=%s tier_bytes=%s threads=%s timeout=%ss\n' \
    "$bytes" "$shape_bytes" "$rounds" "$tiers" "$tier_bytes" "$threads" "$timeout"
