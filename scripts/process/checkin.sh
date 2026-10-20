#!/usr/bin/env bash
# Run every check-in alarm script. Exit 0: no alarm. Exit 1: an alarm. Exit 2: a check could not run.
set -u
dir=${BASH_SOURCE[0]%/*}
[[ ${BASH_SOURCE[0]} == */* ]] || dir=. # run as "bash checkin.sh" from its own directory
codes=() summary= missing=0 unmatched=0

check() {
	echo "== $1"
	"${@:2}"
	local code=$?
	codes+=("$code") summary+=" $1=$code"
}

# notice-gap exits 0 when its only problem rows are unmatched, so count rows by verdict too. The verdict
# is read at its header offset since the last column is free text; the space after each word keeps out
# the footer, which has "unmatched," at that offset when its counts are single digits.
notice_gap() {
	local out code header prefix line
	out=$(python3 "$dir/notice-gap.py")
	code=$?
	[[ $out ]] && printf '%s\n' "$out"
	header=${out%%$'\n'*}
	prefix=${header%%verdict*}
	while IFS= read -r line; do
		case ${line:${#prefix}} in
		"MISSING "*) ((++missing)) ;;
		"unmatched "*) ((++unmatched)) ;;
		esac
	done <<<"$out"
	return "$code"
}

check ready-issues python3 "$dir/ready-issues.py" --instance 2
check pr-cap python3 "$dir/pr-cap.py" --instance 2
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
