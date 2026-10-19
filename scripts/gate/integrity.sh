#!/usr/bin/env bash
# gate/integrity: fails when a pull request weakens the lint gate. It reads only
# `git diff --unified=0 BASE...HEAD` and runs nothing from the pull request.
# One line per finding:
#   - a new suppression: eslint-disable, @ts-ignore, @ts-nocheck or @ts-expect-error
#     in JS/TS, or a Rust allow( / expect( lint use, keyed like
#     rust-lint-attributes.mjs and counted net per (file, key), so moving one inside
#     a file passes;
#   - a growing baseline: the "count" values in eslint-suppressions.json sum higher
#     (a new #[expect(..., reason = "baseline ...")] marker is a new suppression);
#   - an edit to a gate file, or to a lint table or lint level in a Cargo.toml;
#   - more than 500 changed non-test lines, not counting eslint-suppressions.json
#     and package-lock.json.
# The diff is read line by line, so it is weaker than gate/lint: a lint added on its
# own line inside an existing multi-line attribute, or text in a block comment or
# a multi-line string, is not seen.
# Usage: scripts/gate/integrity.sh <base-rev> <head-rev>
set -euo pipefail

if [ $# -ne 2 ]; then
  echo "Usage: scripts/gate/integrity.sh <base-rev> <head-rev>" >&2
  exit 2
fi
for rev in "$1" "$2"; do
  if ! git rev-parse --verify --quiet "$rev^{commit}" > /dev/null; then
    echo "integrity: unknown git revision: $rev" >&2
    exit 2
  fi
done

git -c core.quotePath=false diff --unified=0 --no-color --no-ext-diff --no-textconv --find-renames "$1...$2" | perl -ne '
  BEGIN {
    $GATE_FILE = qr{^(\.github/workflows/gate[^/]*\.ya?ml|scripts/gate/.*|eslint\.config\.js|\.jscpd\.json|clippy\.toml|\.github/rust-version)$};
    $SIZE_EXEMPT = qr{(^|/)(tests?|__tests__)/|\.(test|spec)\.[^/]+$|(^|/)(eslint-suppressions\.json|package-lock\.json)$};
    $JS = qr{\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$};
    $LINT_TOML = qr{^\s*\[(workspace\.)?lints|=\s*"(allow|warn|deny|forbid|expect|force-warn)"|\blevel\s*=|^\s*workspace\s*=\s*true};
    $MAX_LINES = 500;
    ($changed, $suppression_sum) = (0, 0);
  }

  sub finding { push @findings, "integrity: $_[0]" }

  sub rust_keys {
    my ($code) = @_;
    $code =~ s{"(?:\\.|[^"\\])*"|//.*}{ }g;
    my @keys;
    while ($code =~ /#\s*!?\s*\[([^\]]*\$[^\]]*)\]/g) { (my $body = $1) =~ s/\s+//g; push @keys, "attr([$body])" }
    while ($code =~ /(?<![\w.:])(?<!fn )(allow|expect)\s*\(/g) {
      my ($kind, $rest, $depth, $args) = ($1, substr($code, pos($code)), 1, "");
      for my $char (split //, $rest) { $depth += $char eq "(" ? 1 : $char eq ")" ? -1 : 0; last if $depth == 0; $args .= $char }
      my @lints = grep { length && !/^reason/ } map { s/\s+//gr } split /,/, $args;
      push @keys, @lints ? map { "$kind($_)" } @lints : "$kind(<multi-line>)";
    }
    return @keys;
  }

  sub changed_line {
    my ($sign, $text) = @_;
    $changed++ unless $path =~ $SIZE_EXEMPT;
    $suppression_sum += $sign * $1 if $path =~ m{(^|/)eslint-suppressions\.json$} && $text =~ /"count"\s*:\s*(\d+)/;
    $lint_toml{$path} //= $line if $path =~ m{(^|/)Cargo\.toml$} && $text =~ $LINT_TOML;
    my @keys = $path =~ $JS ? $text =~ /(eslint-disable|\@ts-(?:ignore|nocheck|expect-error))/g
      : $path =~ /\.rs$/ ? rust_keys($text)
      : ();
    for my $key (@keys) { $net{"$path\t$key"} += $sign; $first{"$path\t$key"} //= $line if $sign > 0 }
  }

  if (/^diff --git "?a\/(.*?)"? "?b\/(.*?)"?$/) {
    ($old, $path, $in_hunk) = ($1, $2, 0);
    finding("$path: edits a gate file") if $path =~ $GATE_FILE || $old =~ $GATE_FILE;
  } elsif (/^@@ -\d+(?:,\d+)? \+(\d+)/) {
    ($line, $in_hunk) = ($1, 1);
  } elsif ($in_hunk && /^([+-])(.*)$/) {
    my $sign = $1 eq "+" ? 1 : -1;
    changed_line($sign, $2);
    $line++ if $sign > 0;
  }

  END {
    finding("$_:$lint_toml{$_}: edits a lint table or lint level") for sort keys %lint_toml;
    for my $entry (sort keys %net) {
      next if $net{$entry} <= 0;
      my ($file, $key) = split /\t/, $entry;
      finding("$file:$first{$entry}: adds a suppression: $key (+$net{$entry})");
    }
    finding("eslint-suppressions.json: baseline counts grow by $suppression_sum") if $suppression_sum > 0;
    finding("changes $changed non-test lines, more than $MAX_LINES") if $changed > $MAX_LINES;
    print "$_\n" for @findings;
    if (@findings) {
      print "integrity: ", scalar @findings, " finding(s). Each needs a fix, or an override from Tyler for a gate PR.\n";
      exit 1;
    }
    print "integrity: ok, $changed non-test lines changed\n";
  }
'
