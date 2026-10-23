#!/usr/bin/env bash
# Run every check-in alarm script. Exit 0: no alarm. Exit 1: an alarm. Exit 2: a check could not run.
# CHECKIN_HOURS: notice-gap judges only the last N hours (default 2: an hourly check-in with overlap),
# so an old late or missing notice stops the alarm once it leaves the window.
# CHECKIN_TIMEOUT: seconds each script may run (default 120).
set -u
dir=${BASH_SOURCE[0]%/*}
[[ ${BASH_SOURCE[0]} == */* ]] || dir=. # run as "bash checkin.sh" from its own directory
codes=() summary= missing=0 unmatched=0

# perl, since macOS has no timeout command. A script stopped by the alarm exits 142.
script() {
	perl -e 'alarm shift; exec @ARGV or exit 127' "${CHECKIN_TIMEOUT:-120}" python3 "$dir/$1.py" "${@:2}"
}

check() {
	echo "== $1"
	"${@:2}"
	local code=$?
	codes+=("$code") summary+=" $1=$code"
}

# notice-gap exits 0 when its only problem rows are unmatched, so count rows by verdict too. The verdict
# is read at its header offset since the last column is free text; the space after each word keeps out
# the footer, which has "unmatched," at that offset when its counts are single digits. No table (a crash,
# or a format change) means the rows cannot be counted: that is an error, not an alarm.
notice_gap() {
	local out code header prefix line
	out=$(script notice-gap --hours "${CHECKIN_HOURS:-2}")
	code=$?
	[[ $out ]] && printf '%s\n' "$out"
	header=${out%%$'\n'*}
	[[ $header == *" verdict "* ]] || return 2
	prefix=${header%%verdict*}
	while IFS= read -r line; do
		case ${line:${#prefix}} in
		"MISSING "*) ((++missing)) ;;
		"unmatched "*) ((++unmatched)) ;;
		esac
	done <<<"$out"
	return "$code"
}

check ready-issues script ready-issues --instance 1
check pr-cap script pr-cap --instance 2
check notice-gap notice_gap

status=0
for code in "${codes[@]}"; do
	((code > 1)) && code=2
	((code > status)) && status=$code
done
((status == 0 && missing + unmatched > 0)) && status=1
words=(ok alarm error)
echo "checkin: ${words[status]} (${summary# } missing=$missing unmatched=$unmatched)"
exit "$status"
