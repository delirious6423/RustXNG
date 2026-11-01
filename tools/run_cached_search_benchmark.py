#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Paired fresh-process cached-API benchmark; no external network requests.

Build the identical cached_search_benchmark.rs in separate target directories
for the pinned original and the candidate, then supply their binary paths.
Outputs include raw per-request measurements, process memory, A/A controls,
fixture and response hashes, binary hashes, and declared benchmark scope.
"""

import argparse
import hashlib
import json
import pathlib
import random
import statistics
import subprocess


def median(values):
    return statistics.median(values)


def interval(values, rng):
    samples = sorted(median(rng.choices(values, k=len(values))) for _ in range(2000))
    return [samples[49], samples[1949]]


def run(binary, workload, iterations, html_dir, transport):
    command = [str(binary), "--workload", workload, "--iterations", str(iterations)]
    command.extend(["--transport", transport])
    if workload == "real":
        command.extend(["--html-dir", str(html_dir)])
    completed = subprocess.run(
        command,
        check=True, capture_output=True, text=True, timeout=120,
    )
    result = json.loads(completed.stdout)
    assert result["cache"]["miss"] == 0
    assert result["cache"]["hit"] == 6 * (iterations + result["warmup_requests"])
    assert result["engine_errors"] == 0
    return result


def latency(result):
    return median(sample["elapsed_ns"] for sample in result["samples"]) / 1_000_000


def rss(result, field):
    return result["memory_after_requests"][field]


def percentile(values, fraction):
    values = sorted(values)
    index = max(0, min(len(values) - 1, int((len(values) - 1) * fraction)))
    return values[index]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=pathlib.Path, required=True)
    parser.add_argument("--candidate", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--rounds", type=int, default=6)
    parser.add_argument("--iterations", type=int, default=30)
    parser.add_argument("--seed", type=int, default=527411)
    parser.add_argument("--html-dir", type=pathlib.Path)
    parser.add_argument("--transport", choices=["in-process", "http"], default="in-process")
    args = parser.parse_args()
    assert args.rounds >= 3 and args.iterations > 0
    baseline = args.baseline.resolve(strict=True)
    candidate = args.candidate.resolve(strict=True)
    rng = random.Random(args.seed)
    output = {
        "schema_version": 1,
        "transport": args.transport,
        "seed": args.seed, "rounds": args.rounds, "iterations_per_process": args.iterations,
        "baseline_binary": str(baseline), "candidate_binary": str(candidate),
        "baseline_binary_sha256": hashlib.sha256(baseline.read_bytes()).hexdigest(),
        "candidate_binary_sha256": hashlib.sha256(candidate.read_bytes()).hexdigest(),
        "scope": "Single caller; one Mwmbl engine; five cached page bodies; actual Axum routing, parsing, ranking, extraction, JSON/body collection. HTTP transport includes loopback TCP/HTTP; in-process transport excludes it. Both exclude TLS and live upstreams.",
        "method": "Randomized paired fresh-process A/B, randomized A/A identity controls, three warmup requests per process. Time ratios use process medians. Bootstrap intervals describe these paired runs, not production variation. VmRSS and VmHWM include benchmark setup and allocator retention; neither is extraction-only memory.",
        "workloads": [],
    }
    for workload in ["standard", "mixed"] + (["real"] if args.html_dir else []):
        rounds = []
        identity = None
        for round_index in range(args.rounds):
            order = ["baseline", "candidate"]
            rng.shuffle(order)
            aa_order = ["a", "b"]
            rng.shuffle(aa_order)
            phases = ["ab", "aa"]
            rng.shuffle(phases)
            results = {}
            for phase in phases:
                for label in order if phase == "ab" else aa_order:
                    binary = candidate if label == "candidate" else baseline
                    result = run(binary, workload, args.iterations, args.html_dir, args.transport)
                    observed_identity = (result["input_sha256"], result["output_sha256"])
                    if identity is None:
                        identity = observed_identity
                    assert observed_identity == identity, "old/new inputs or full API output differ"
                    results[label] = result
            row = {
                "round": round_index, "phase_order": phases,
                "ab_order": order, "aa_order": aa_order,
                "ab_ratio": latency(results["baseline"]) / latency(results["candidate"]),
                "aa_ratio": latency(results["a"]) / latency(results["b"]),
                "runs": results,
            }
            rounds.append(row)
            print(f"{workload} round {round_index + 1}/{args.rounds}: A/B={row['ab_ratio']:.3f} A/A={row['aa_ratio']:.3f}", flush=True)
        old = [row["runs"]["baseline"] for row in rounds]
        new = [row["runs"]["candidate"] for row in rounds]
        ab = [row["ab_ratio"] for row in rounds]
        aa = [row["aa_ratio"] for row in rounds]
        old_samples = [sample["elapsed_ns"] / 1_000_000 for result in old for sample in result["samples"]]
        new_samples = [sample["elapsed_ns"] / 1_000_000 for result in new for sample in result["samples"]]
        summary = {
            "baseline_process_median_ms": median([latency(result) for result in old]),
            "candidate_process_median_ms": median([latency(result) for result in new]),
            "paired_median_speedup": median(ab), "paired_bootstrap_95_percent": interval(ab, rng),
            "aa_median_ratio": median(aa), "aa_bootstrap_95_percent": interval(aa, rng),
            "baseline_request_p95_ms": percentile(old_samples, .95),
            "candidate_request_p95_ms": percentile(new_samples, .95),
            "baseline_median_rss_kib": median([rss(result, "rss_kib") for result in old]),
            "candidate_median_rss_kib": median([rss(result, "rss_kib") for result in new]),
            "baseline_median_hwm_kib": median([rss(result, "high_water_kib") for result in old]),
            "candidate_median_hwm_kib": median([rss(result, "high_water_kib") for result in new]),
            "input_sha256": identity[0], "output_sha256": identity[1],
            "full_output_equal": True,
        }
        output["workloads"].append({"name": workload, "summary": summary, "rounds": rounds})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    print(json.dumps([{ "name": row["name"], **row["summary"]} for row in output["workloads"]], indent=2))


if __name__ == "__main__":
    main()
