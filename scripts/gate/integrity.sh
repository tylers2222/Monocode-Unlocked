#!/usr/bin/env bash
# gate/integrity: fails when a pull request weakens the lint gate. It reads only git
# objects (git diff between the merge base and HEAD, and git show of touched Rust
# files) and runs nothing from the pull request. One line per finding:
#   - a new suppression: eslint-disable, @ts-ignore, @ts-nocheck or @ts-expect-error
#     in JS/TS, or a Rust allow( / expect( lint use, keyed like
#     rust-lint-attributes.mjs and counted net per (file, key), so moving one inside
#     a file passes (a new #[expect(..., reason = "baseline ...")] marker counts);
#   - a growing baseline: the "count" values in eslint-suppressions.json sum higher;
#   - an edit to a gate file (workflows, gate and setup scripts, lint and toolchain
#     configs and .npmrc at any depth), to a lint table or lint level in a Cargo.toml,
#     or to a package.json lifecycle script that npm ci runs (dependencies,
#     preinstall, install, postinstall, prepublish, preprepare, prepare, postprepare);
#   - a JS/TS, Rust or Cargo.toml file that git diffs as binary, or a file name with
#     an escape git does not document;
#   - more than 500 changed non-test lines. Tests, fixtures, Rust #[cfg(test)] items,
#     eslint-suppressions.json and package-lock.json do not count.
# The diff is read line by line, so it is weaker than gate/lint: a lint added on its
# own line inside an existing multi-line attribute, or text in a block comment or a
# multi-line string, is not seen.
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
if ! merge_base=$(git merge-base "$1" "$2"); then
  echo "integrity: $1 and $2 have no merge base" >&2
  exit 2
fi

exec perl -e '
  use strict;
  use warnings;

  my ($merge_base, $head) = @ARGV;
  my $GATE_FILE = qr{^(\.github/(workflows|actions)/.*|scripts/gate/.*|scripts/install-linux-deps-[^/]*\.sh|\.github/rust-version|rust-toolchain(\.toml)?)$|(^|/)(eslint\.config\.[cm]?[jt]s|\.?clippy\.toml|\.jscpd\.json|\.cargo/config(\.toml)?|\.npmrc)$};
  my $SIZE_EXEMPT = qr{(^|/)(tests?|__tests__|fixtures?)/|\.(test|spec|fixtures?)\.[^/]+$|(^|/)(eslint-suppressions\.json|package-lock\.json)$};
  my $JS = qr{\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$};
  my $SOURCE = qr{\.(ts|tsx|js|jsx|mjs|cjs|mts|cts|rs)$|(^|/)Cargo\.toml$};
  # Changed lines in these files that can switch gate/lint off: Cargo lint tables and
  # levels, and the npm lifecycle scripts that `npm ci` runs before ESLint and clippy.
  my @GATE_SETTINGS = (
    [qr{(^|/)Cargo\.toml$}, qr{^\s*\[(workspace\.)?lints|=\s*"(allow|warn|deny|forbid|expect|force-warn)"|\blevel\s*=|^\s*workspace\s*=\s*true}],
    [qr{(^|/)package\.json$}, qr{"((pre|post)?(install|prepare)|prepublish|dependencies)"\s*:\s*"}],
  );
  my $MAX_LINES = 500;
  my @DIFF = ("diff", "--no-color", "--no-ext-diff", "--no-textconv", "--find-renames");

  my (@findings, %net, %first, %setting, %cfg_test);
  my ($changed, $suppression_sum) = (0, 0);

  # Control characters are printed as \xNN, so a file name cannot start a workflow command.
  sub finding { push @findings, "integrity: " . ($_[0] =~ s/([\x00-\x1f\x7f])/sprintf("\\x%02x", ord $1)/ger) }

  sub git {
    open(my $out, "-|", "git", @_) or die "git: $!";
    local $/;
    my $text = <$out> // "";
    close $out or die "integrity: git $_[0] failed\n";
    return $text;
  }

  # Lines (1-based) from a #[cfg(test)] attribute to the first later line at the same
  # indent that ends in "}", ";" or ",": the item it applies to in rustfmt layout,
  # whether the item takes one line or many.
  sub cfg_test_lines {
    my @lines = split /\n/, git("show", "$_[0]:$_[1]");
    my %inside;
    for (my $i = 0; $i < @lines; $i++) {
      next unless $lines[$i] =~ /^(\s*)#\[cfg\(test\)\]\s*$/;
      my ($indent, $j) = (length $1, $i + 1);
      $j++ until $j >= @lines
        || ($lines[$j] =~ /^(\s*)(\S.*?)\s*$/ && length($1) == $indent && $2 !~ /^#/ && $2 =~ /[};,]$/);
      $inside{$_} = 1 for $i + 1 .. $j + 1;
      $i = $j;
    }
    return \%inside;
  }

  sub in_cfg_test {
    my ($rev, $path, $line) = @_;
    return ($cfg_test{"$rev:$path"} //= cfg_test_lines($rev, $path))->{$line};
  }

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

  # Paths, renames and binary diffs, from NUL-separated output so any file name parses.
  my @entries = split /\0/, git(@DIFF, "--numstat", "-z", $merge_base, $head);
  while (@entries) {
    my ($added, undef, $path) = split /\t/, shift(@entries), 3;
    my @paths = $path eq "" ? splice(@entries, 0, 2) : ($path);
    my @gate = grep { $_ =~ $GATE_FILE } @paths;
    finding("$gate[-1]: edits a gate file") if @gate;
    finding("$paths[-1]: diffs as binary, so its lines are not checked") if $added eq "-" && grep { $_ =~ $SOURCE } @paths;
  }

  # A --- or +++ path. git C-quotes a name with control characters, so decode its
  # escapes; an escape git does not document is a finding, not a guess.
  my %UNESCAPE = (a => "\a", b => "\b", t => "\t", n => "\n", v => "\x0b", f => "\f", r => "\r", q{"} => q{"}, "\\" => "\\");
  sub diff_path {
    my ($name) = @_;
    $name =~ s/\t$//;
    return undef if $name eq "/dev/null";
    if ($name =~ /^"(.*)"$/) {
      my $unknown;
      $name = $1 =~ s{\\([0-7]{3}|.)}{length $1 == 3 ? chr oct $1 : $UNESCAPE{$1} // ($unknown = "\\$1")}ger;
      finding("$name: file name has an escape git does not document") if $unknown;
    }
    return $name =~ s{^[ab]/}{}r;
  }

  my ($old, $path, $in_hunk, $old_line, $new_line);
  open(my $diff, "-|", "git", "-c", "core.quotePath=false", @DIFF, "--unified=0", "--src-prefix=a/", "--dst-prefix=b/", $merge_base, $head)
    or die "git: $!";
  while (<$diff>) {
    chomp;
    if (/^diff --git /) {
      ($old, $path, $in_hunk) = (undef, undef, 0);
    } elsif (!$in_hunk && /^--- (.*)$/) {
      $old = diff_path($1);
    } elsif (!$in_hunk && /^\+\+\+ (.*)$/) {
      $path = diff_path($1) // $old;
      $old //= $path;
    } elsif (/^@@ -(\d+)(?:,\d+)? \+(\d+)/) {
      ($old_line, $new_line, $in_hunk) = ($1, $2, 1);
    } elsif ($in_hunk && /^([+-])(.*)$/) {
      my ($sign, $text) = ($1 eq "+" ? 1 : -1, $2);
      my @at = $sign > 0 ? ($head, $path, $new_line) : ($merge_base, $old, $old_line);
      $changed++ unless $path =~ $SIZE_EXEMPT || ($path =~ /\.rs$/ && in_cfg_test(@at));
      $suppression_sum += $sign * $1 if $path =~ m{(^|/)eslint-suppressions\.json$} && $text =~ /"count"\s*:\s*(\d+)/;
      for (@GATE_SETTINGS) { $setting{$path} //= $new_line if $path =~ $_->[0] && $text =~ $_->[1] }
      my @keys = $path =~ $JS ? $text =~ /(eslint-disable|\@ts-(?:ignore|nocheck|expect-error))/g
        : $path =~ /\.rs$/ ? rust_keys($text)
        : ();
      for my $key (@keys) { $net{"$path\0$key"} += $sign; $first{"$path\0$key"} //= $new_line if $sign > 0 }
      $sign > 0 ? $new_line++ : $old_line++;
    }
  }
  close $diff or die "integrity: git diff failed\n";

  finding("$_:$setting{$_}: edits a lint or install setting that gate/lint depends on") for sort keys %setting;
  for my $entry (sort keys %net) {
    next if $net{$entry} <= 0;
    my ($file, $key) = split /\0/, $entry;
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
' "$merge_base" "$2"
