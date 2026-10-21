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
