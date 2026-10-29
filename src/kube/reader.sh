# rtop reader: answers one request per line on stdin with framed /proc
# contents on stdout (specs/001-k8s-targets/contracts/reader-wire.md).
#
# rtop writes this script as the first input of `sh` inside a container, so
# it must work with whatever shell the image has: POSIX sh only, tested with
# dash and busybox ash. Per process it uses builtins (read, printf) and never
# forks, so a snapshot of a few hundred processes stays cheap.
#
# Node mode sets RTOP_ROOT=/proc/1/root and RTOP_NET=/proc/1/net first, so
# user names and network counters come from the host, not the helper pod.
#
# Everything sits in one function that is called on the last line: the
# shell parses the whole script before it prints the hello, so the first
# request can never end up in the parser's buffer instead of `read`.

rtop_reader() {
	# Frame start, frame end with an exit status, end of an answer.
	_f() { printf '\036F %s\n' "$1"; }
	_e() { printf '\036E %s\n' "$1"; }
	_d() { printf '\036D %s\n' "$1"; }
	# One file as one frame, named by $2 or by its path.
	_file() {
		_f "${2:-$1}"
		cat "$1" 2>/dev/null
		_e $?
	}
	# A text file as one frame, read with builtins (no fork per file).
	_text() {
		_f "$2"
		rc=1
		{
			while IFS= read -r l; do
				printf '%s\n' "$l"
				rc=0
			done <"$1"
		} 2>/dev/null
		_e $rc
	}
	# `L <name> <target>` for every symlink in a directory.
	_links() {
		for l in "$1"/*; do
			[ -L "$l" ] && printf 'L %s %s\n' "${l##*/}" "$(readlink "$l" 2>/dev/null)"
		done
	}

	_hello() {
		_f version
		printf '1\n'
		_e 0
		_f pid
		printf '%s\n' $$
		_e 0
		_f uname
		uname -r 2>/dev/null
		_e $?
		_file "${RTOP_ROOT:-}/etc/passwd" passwd
		_f pagesize
		getconf PAGESIZE 2>/dev/null
		_e $?
		# getconf is often missing; rtop derives the page size from these.
		_file /proc/$$/statm statm
		_file /proc/$$/status status
		_d 0
	}

	_snap() {
		# The loop below reuses the positional parameters.
		seq=$1 cgroot=${2:-}
		for x in stat meminfo loadavg uptime; do
			_file /proc/$x
		done
		_file "${RTOP_NET:-/proc/net}/dev" /proc/net/dev
		_file /proc/diskstats
		for d in /proc/[0-9]*; do
			# A process may exit between the glob and the reads; the shell
			# reports the failed open on stderr before any inner 2>/dev/null
			# applies, hence the outer braces.
			{ read -r st <"$d/stat"; } 2>/dev/null || continue
			# Real UID: the first number on the Uid: line of status.
			uid=-1
			{
				while read -r k v _; do
					[ "$k" = Uid: ] && uid=$v && break
				done <"$d/status"
			} 2>/dev/null
			# A glob that matches nothing stays literal: then fd/ is empty or
			# unreadable, and only the second case is unknown.
			set -- "$d"/fd/*
			n=$#
			if [ ! -e "$1" ] && [ ! -L "$1" ]; then
				if [ -r "$d/fd" ]; then n=0; else n=-1; fi
			fi
			printf '\036F p %s\n%s\n%s\n%s\n\036E 0\n' "${d#/proc/}" "$st" "$uid" "$n"
		done
		# The container's own cgroup: its usage against its limits.
		if [ -n "$cgroot" ]; then
			for x in cpu.max cpu.stat memory.current memory.max memory.peak memory.events; do
				_text "$cgroot/$x" "cg $x"
			done
		fi
		_d "$seq"
	}

	_cmd() {
		seq=$1
		shift
		# argv, and the cgroup that says which container the process is in.
		# A node has hundreds of processes: the cgroup is read with the
		# builtin read (no fork); argv may contain NULs, so it takes cat.
		for p in "$@"; do
			_file /proc/$p/cmdline
			_text /proc/$p/cgroup /proc/$p/cgroup
		done
		_d "$seq"
	}

	_details() {
		d=/proc/$2
		for x in stat status cmdline environ io limits cgroup schedstat smaps_rollup maps wchan oom_score oom_score_adj; do
			_file "$d/$x" $x
		done
		for x in exe cwd; do
			_f $x
			readlink "$d/$x" 2>/dev/null
			_e $?
		done
		_f ns
		_links "$d/ns"
		_e 0
		_f self_ns
		_links /proc/$$/ns
		_e 0
		_f fd
		if [ -r "$d/fd" ]; then
			_links "$d/fd"
			_e 0
		else
			_e 1
		fi
		for t in "$d"/task/*; do
			_file "$t/stat" "t ${t##*/}"
		done
		for x in tcp tcp6 udp udp6 unix; do
			_file "$d/net/$x" net/$x
		done
		# cgroup v2: the files of the process's own cgroup directory.
		cg=
		{
			while IFS= read -r l; do
				case $l in 0::*) cg=${l#0::} ;; esac
			done <"$d/cgroup"
		} 2>/dev/null
		if [ -n "$cg" ]; then
			for x in memory.current memory.max memory.peak memory.pressure cpu.max cpu.weight cpu.pressure \
				pids.current pids.max io.pressure; do
				[ -e "/sys/fs/cgroup$cg/$x" ] && _file "/sys/fs/cgroup$cg/$x" "cg $x"
			done
		fi
		_d "$1"
	}

	# kill <seq> <SIGNAL> <pid>: only sent when rtop runs with --allow-actions.
	_kill() {
		_f rc
		kill -s "$2" "$3" 2>&1
		_e $?
		_d "$1"
	}

	_hello
	while read -r req seq rest; do
		case $req in
		snap) _snap "$seq" $rest ;;
		cmd) _cmd "$seq" $rest ;;
		details) _details "$seq" $rest ;;
		kill) _kill "$seq" $rest ;;
		bye) exit 0 ;;
		*) _d "${seq:-0}" ;;
		esac
	done
}
rtop_reader
