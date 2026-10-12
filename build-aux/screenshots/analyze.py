"""analyze: summarise the readout sweeps the noise-atlas notes plot.

Reads the CSV files build.sh writes, one per shot count, groups their rows
by backend and prints the mean fidelity of each with a bootstrap interval.
"""

import argparse
import csv
import random
import statistics
import sys
from collections import defaultdict
from pathlib import Path

SHOTS = (128, 1024, 8192)
RESAMPLES = 2000
LEVEL = 0.95
SEED = 7


def read_sweep(path):
    """The rows of one sweep file, as (backend, fidelity) pairs."""
    with open(path, newline="") as f:
        for row in csv.DictReader(f):
            yield row["backend"], float(row["fidelity"])


def by_backend(paths):
    """Every fidelity measured, grouped by the backend it was measured on."""
    groups = defaultdict(list)
    for path in paths:
        for backend, fidelity in read_sweep(path):
            groups[backend].append(fidelity)
    return groups


def bootstrap(values, resamples=RESAMPLES, level=LEVEL, seed=SEED):
    """A percentile bootstrap interval for the mean of `values`."""
    rng = random.Random(seed)
    means = sorted(
        statistics.fmean(rng.choices(values, k=len(values)))
        for _ in range(resamples)
    )
    tail = (1 - level) / 2
    return means[int(tail * resamples)], means[int((1 - tail) * resamples) - 1]


def summarise(groups):
    """One row per backend: how many runs, their mean and its interval."""
    rows = []
    for backend, values in sorted(groups.items()):
        low, high = bootstrap(values)
        rows.append(
            {
                "backend": backend,
                "runs": len(values),
                "mean": statistics.fmean(values),
                "low": low,
                "high": high,
                "stdev": statistics.stdev(values) if len(values) > 1 else 0.0,
            }
        )
    return rows


def print_table(rows, out=sys.stdout):
    """The summary as a Markdown table, which the notes paste as it is."""
    out.write("| backend | runs | mean | 95 % interval | stdev |\n")
    out.write("| ------- | ---- | ---- | ------------- | ----- |\n")
    for r in rows:
        out.write(
            f"| {r['backend']} | {r['runs']} | {r['mean']:.3f} "
            f"| {r['low']:.3f} to {r['high']:.3f} | {r['stdev']:.3f} |\n"
        )


def sweep_files(folder, shots):
    """The sweep files build.sh wrote into `folder` for these shot counts."""
    paths = [Path(folder) / f"sweep-{n}.csv" for n in shots]
    missing = [p for p in paths if not p.exists()]
    if missing:
        sys.exit(f"no sweep yet: {', '.join(map(str, missing))} (run build.sh)")
    return paths


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("folder", nargs="?", default="../Attachments")
    parser.add_argument("--shots", type=int, nargs="*", default=SHOTS)
    args = parser.parse_args(argv)
    groups = by_backend(sweep_files(args.folder, args.shots))
    print_table(summarise(groups))


if __name__ == "__main__":
    main()
