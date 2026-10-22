#!/bin/sh
# Design system check (docs/v2/DESIGN-SYSTEM.md, skill fleet-design-system).
# Area style sheets and components use the tokens of web/src/styles/tokens.css,
# never raw values. Prints each violation as file:line and exits non-zero if
# one exists. A line that carries the comment `ds-exception: <reason>` is
# allowed (the skill says when an exception is right); the script lists them.
#
#   sh tools/design-check.sh          # check
#   sh tools/design-check.sh --list   # also print every ds-exception line

set -u
cd "$(dirname "$0")/.." || exit 2

styles=web/src/styles
fail=0

# report <title> <grep output>
report() {
    if [ -n "$2" ]; then
        echo "$1:"
        echo "$2" | sed 's/^/  /'
        fail=1
    fi
}

# Area style sheets: every CSS file but tokens.css (the only home of raw values).
area=$(find "$styles" -name '*.css' ! -name 'tokens.css' | sort)

# 1. Raw colors (hex, rgb(), hsl()) outside tokens.css.
report "raw color (use a color token)" "$(grep -nE '#[0-9a-fA-F]{3,8}\b|rgba?\(|hsla?\(' $area | grep -v 'ds-exception')"

# 2. Font families other than the font tokens.
report "font family (use --font-ui or --font-data)" "$(grep -nE 'font-family:' $area | grep -v 'var(--font-' | grep -vE 'inherit|ds-exception')"

# 3. Font sizes as literal lengths.
report "font size (use an --fs-* token)" "$(grep -nE 'font-size: *[0-9.]+(px|rem|em)' $area | grep -v 'ds-exception')"

# 4. Capitals: labels are in sentence case.
report "text-transform uppercase (labels are sentence case)" "$(grep -nE 'text-transform: *uppercase' $area | grep -v 'ds-exception')"

# 5. The model and provider hues are retired: models are never colored.
report "model or provider hue (models are not colored)" "$(grep -nE 'var\(--(m|p)-[a-z]+' $area | grep -v 'ds-exception')"

# 5b. A shadow token is never `none`: it sits in box-shadow lists, where `…, none`
# is invalid and drops the whole declaration. tokens.css is not in $area, so name it.
report "shadow token set to none (use 0 0 0 0 transparent: it sits in box-shadow lists)" \
    "$(grep -nE -- '--(shadow-lift|selected-lift|knob-shadow|shadow-pop): *none' web/src/styles/tokens.css web/src/ui/palette.ts)"

# 5c. Touch targets. The `@media (pointer: coarse)` rule of base.css gives buttons and summaries
# 44px through `:where()`, at zero specificity: an area rule that sets `min-height: 0` on a
# button, a summary or a button class (`-btn`, `.fd-link`, `.fd-dl-n`) silently drops the target.
# Such a rule needs a `@media (pointer: coarse)` rule in the same file that names the same
# selector (and gives the target back). The awk parser below reads one file; it ignores comments
# and splits selector lists at the top-level commas.
tap_awk='
function trim(s) { gsub(/^[ \t\n]+|[ \t\n]+$/, "", s); gsub(/[ \t\n]+/, " ", s); return s }
function split_sel(s, parts,    n, i, c, depth, cur) {
    n = 0; depth = 0; cur = ""
    for (i = 1; i <= length(s); i++) {
        c = substr(s, i, 1)
        if (c == "(") depth++
        else if (c == ")") depth--
        if (c == "," && depth == 0) { parts[++n] = trim(cur); cur = "" } else cur = cur c
    }
    parts[++n] = trim(cur)
    return n
}
function decl(d) { if (sp > 0 && kind[sp] == "rule" && d ~ /^min-height: *0(px)?$/) min0[sp] = 1 }
{
    line = $0 "\n"
    for (i = 1; i <= length(line); i++) {
        c = substr(line, i, 1)
        if (incomment) {
            if (c == "*" && substr(line, i + 1, 1) == "/") { incomment = 0; i++ }
            continue
        }
        if (c == "/" && substr(line, i + 1, 1) == "*") { incomment = 1; i++; continue }
        if (c == "{") {
            pre = trim(buf); buf = ""
            sp++
            if (pre ~ /^@media/) { kind[sp] = "media"; coarse[sp] = (pre ~ /pointer: *coarse/) }
            else if (pre ~ /^@/) { kind[sp] = "at"; coarse[sp] = 0 }
            else { kind[sp] = "rule"; coarse[sp] = 0; sel[sp] = pre; min0[sp] = 0; at[sp] = startline }
            continue
        }
        if (c == "}") {
            decl(trim(buf)); buf = ""
            if (sp > 0 && kind[sp] == "rule") {
                inc = 0
                for (k = 1; k < sp; k++) if (coarse[k]) inc = 1
                n = split_sel(sel[sp], parts)
                for (k = 1; k <= n; k++) {
                    if (inc) coarse_sel[parts[k]] = 1
                    else if (min0[sp]) { cand[++nc] = parts[k]; cand_line[nc] = at[sp] }
                }
            }
            if (sp > 0) sp--
            continue
        }
        if (c == ";") { decl(trim(buf)); buf = ""; continue }
        if (trim(buf) == "" && c !~ /[ \t\n]/) startline = FNR
        buf = buf c
    }
}
END {
    for (k = 1; k <= nc; k++) {
        s = cand[k]
        if (s !~ /button|summary|\.fd-link|\.fd-dl-n|-btn/) continue
        if (s in coarse_sel) continue
        skip = 0
        for (a = 1; a <= nallow; a++) if (index(s, allow[a]) > 0) skip = 1
        if (!skip) print FILENAME ":" cand_line[k] ": " s
    }
}
BEGIN { nallow = split(ALLOW, allow, "|") }
'
# Allowlist: inline controls that sit in a line of text or code and keep the line height on
# purpose (the "inline" exception of WCAG 2.5.8), so no coarse rule names them. A selector that
# contains an entry passes.
#   .fd-cchip               a comment reference chip inside chat prose (session.css)
#   .fd-plan-meta .fd-link  a text button inside the meta line of a plan (stations.css)
#   .fd-dl-n                the line-number comment button in a diff gutter (session.css, chat.css)
tap_allow='.fd-cchip|.fd-plan-meta .fd-link|.fd-dl-n'
tap_out=""
for f in $(find "$styles" -name '*.css' ! -name 'base.css' ! -name 'tokens.css' | sort); do
    tap_out="$tap_out$(awk -v ALLOW="$tap_allow" "$tap_awk" "$f")
"
done
report "min-height: 0 on a button or summary without a pointer: coarse rule (touch targets are 44px)" \
    "$(printf '%s' "$tap_out" | sed '/^$/d')"

# 6. Components: no raw colors or font sizes in inline styles.
tsx=$(find web/src -name '*.tsx' ! -name '*.test.tsx' ! -path '*/gen/*')
report "inline style with a raw color or size (use a class and tokens)" \
    "$(grep -nE "style=\{\{[^}]*(#[0-9a-fA-F]{3,8}|rgba?\(|fontSize|fontFamily)" $tsx | grep -v 'ds-exception')"

if [ "${1:-}" = "--list" ]; then
    echo "ds-exception lines:"
    grep -rn 'ds-exception' "$styles" web/src --include='*.css' --include='*.tsx' | sed 's/^/  /'
fi

count=$(grep -rn 'ds-exception' web/src --include='*.css' --include='*.tsx' | wc -l | tr -d ' ')
if [ "$fail" -ne 0 ]; then
    echo "design check: FAILED (see docs/v2/DESIGN-SYSTEM.md and .claude/skills/fleet-design-system/SKILL.md)"
    exit 1
fi
echo "design check: every style sheet and component uses the design tokens ($count ds-exception lines)"
