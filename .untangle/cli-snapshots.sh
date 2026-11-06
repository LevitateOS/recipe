#!/usr/bin/env bash
# Capture CLI behavior snapshots. Usage: bash .untangle/cli-snapshots.sh <recipe-bin> <out-dir>  (from tools/recipe)
B=$1; O=$2; mkdir -p "$O"
$B --help > $O/help.txt 2>&1; echo $? >> $O/help.txt
$B --version > $O/version.txt 2>&1
for c in install remove cleanup isinstalled isbuilt isacquired list info hash; do $B $c --help > $O/help-$c.txt 2>&1; done
($B info examples/hello.rhai; echo "exit=$?") > $O/info-hello.txt 2>&1
($B -r examples list; echo "exit=$?") > $O/list-examples.txt 2>&1
($B hash examples/hello.rhai; echo "exit=$?") > $O/hash-hello.txt 2>&1
(env -u GINUX_BIN $B install examples/hello.rhai --no-persist-state --build-dir "$O/hello-work"; echo "exit=$?") > $O/install-hello-unprivileged.txt 2>&1
