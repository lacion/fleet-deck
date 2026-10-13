#!/bin/sh
# R9.5 (DL-162): prints the board HTTP route map as a Markdown table,
# generated from the method table of the deck
# (crates/fleetdeck-deck/src/http/methods.rs): audit name, HTTP method, route,
# required scope and whether the route mutates (CSRF and an audit row).
#
#   sh tools/route-map.sh                 # print the table
#   sh tools/route-map.sh > /tmp/map.md   # then paste it into docs/v2/internals.md
#
# It reads the source only; it builds and runs nothing.

set -u
cd "$(dirname "$0")/.." || exit 2
src=crates/fleetdeck-deck/src/http/methods.rs

awk '
    /^pub const API: &\[ApiMethod\] = &\[/ { on = 1; next }
    on && /^\];/ { on = 0 }
    on {
        sub(/\/\/.*/, "")          # drop comments
        gsub(/[ \t]+/, "")          # no name or path has a space
        buf = buf $0
    }
    END {
        gsub(/,\)/, ")", buf)       # a trailing comma before a closing bracket
        # One entry ends with `true)` or `false)` (m) or `Scope::X)` (cmd).
        gsub(/(true|false)\),/, "&\n", buf)
        gsub(/cmd\("[a-z_.]+",Scope::[A-Za-z]+\),/, "&\n", buf)
        n = split(buf, parts, "\n")
        print "| Name | Method | Route | Scope | Mutating |"
        print "| --- | --- | --- | --- | --- |"
        for (i = 1; i <= n; i++) {
            p = parts[i]
            if (p ~ /^cmd\(/) {
                sub(/^cmd\(/, "", p); sub(/\),?$/, "", p)
                split(p, a, ",")
                gsub(/"/, "", a[1]); sub(/Scope::/, "", a[2])
                row(a[1], "POST", "/api/commands/" a[1], tolower(a[2]), "yes")
            } else if (p ~ /^m\(/) {
                sub(/^m\(/, "", p); sub(/\),?$/, "", p)
                split(p, a, ",")
                name = a[1]
                if (name == "WS_METHOD") name = "board.ws"
                if (name == "SHELL_WS_METHOD") name = "shells.attach"
                gsub(/"/, "", name)
                method = a[2]; sub(/Method::/, "", method)
                path = a[3]; gsub(/"/, "", path)
                scope = a[4]
                if (scope == "None") scope = "none (login)"
                else { sub(/Some\(Scope::/, "", scope); sub(/\)$/, "", scope); scope = tolower(scope) }
                row(name, method, path, scope, (a[5] == "true") ? "yes" : "no")
            }
        }
    }
    function row(name, method, path, scope, mutating) {
        printf "| `%s` | %s | `%s` | %s | %s |\n", name, method, path, scope, mutating
    }
' "$src"
