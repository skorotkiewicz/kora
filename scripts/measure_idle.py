#!/usr/bin/env python3
"""Measure one existing Linux process without changing the desktop session."""
import argparse
import json
import os
from pathlib import Path
import time


def snapshot(pid):
    # comm can contain spaces and parentheses; numeric fields follow the last ')'.
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (
        int(fields[19]),  # process start time, to detect PID reuse
        int(fields[11]) + int(fields[12]),  # user + system CPU ticks
        int(fields[21]) * os.sysconf("SC_PAGE_SIZE"),  # RSS bytes
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int)
    parser.add_argument("--settle", type=float, default=30)
    parser.add_argument("--seconds", type=float, default=60)
    args = parser.parse_args()
    if args.pid <= 0 or args.settle < 0 or args.seconds <= 0:
        parser.error("PID and seconds must be positive; settle must not be negative")
    try:
        identity = snapshot(args.pid)[0]
        time.sleep(args.settle)
        before = snapshot(args.pid)
        start = time.monotonic()
        time.sleep(args.seconds)
        after = snapshot(args.pid)
        elapsed = time.monotonic() - start
        if before[0] != identity or after[0] != identity:
            parser.error("process exited and its PID was reused; measurement invalid")
    except (OSError, ValueError, IndexError) as error:
        parser.exit(1, f"Cannot measure process {args.pid}: {error}\n")
    cpu_seconds = (after[1] - before[1]) / os.sysconf("SC_CLK_TCK")
    print(json.dumps({
        "pid": args.pid,
        "elapsed_seconds": round(elapsed, 3),
        "cpu_seconds": cpu_seconds,
        "percent_of_one_cpu": round(100 * cpu_seconds / elapsed, 3),
        "rss_start_mib": round(before[2] / 1024**2, 2),
        "rss_end_mib": round(after[2] / 1024**2, 2),
        "note": "RSS endpoints, not peak usage; redraw activity is not measured",
    }, indent=2))


if __name__ == "__main__":
    main()
