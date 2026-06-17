from __future__ import annotations

import argparse
import json
import math
import random
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Callable, Iterable


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None

    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]

    index = (len(ordered) - 1) * quantile
    lower = math.floor(index)
    upper = math.ceil(index)
    if lower == upper:
        return ordered[lower]

    fraction = index - lower
    return ordered[lower] + (ordered[upper] - ordered[lower]) * fraction


def bootstrap_ci(
    values: list[float],
    statistic: Callable[[list[float]], float | None],
    *,
    iterations: int,
    seed: int,
) -> list[float] | None:
    if len(values) < 2:
        point = statistic(values)
        return [point, point] if point is not None else None

    generator = random.Random(seed)
    estimates: list[float] = []
    for _ in range(iterations):
        sample = [values[generator.randrange(len(values))] for _ in values]
        estimate = statistic(sample)
        if estimate is not None:
            estimates.append(estimate)

    low = percentile(estimates, 0.025)
    high = percentile(estimates, 0.975)
    if low is None or high is None:
        return None
    return [low, high]


def distribution_stats(
    values: list[float],
    *,
    bootstrap_iterations: int,
    seed: int,
) -> dict[str, object]:
    if not values:
        return {"runs": 0}

    return {
        "runs": len(values),
        "mean": statistics.fmean(values),
        "median": statistics.median(values),
        "p95": percentile(values, 0.95),
        "min": min(values),
        "max": max(values),
        "stdev": statistics.stdev(values) if len(values) > 1 else 0.0,
        "ci95_mean": bootstrap_ci(
            values,
            statistics.fmean,
            iterations=bootstrap_iterations,
            seed=seed,
        ),
        "ci95_p95": bootstrap_ci(
            values,
            lambda sample: percentile(sample, 0.95),
            iterations=bootstrap_iterations,
            seed=seed + 1,
        ),
    }


def geomean(values: Iterable[float]) -> float | None:
    positive = [value for value in values if value > 0]
    if not positive:
        return None
    return math.exp(statistics.fmean(math.log(value) for value in positive))


def exit_code_summary(exit_codes: list[int]) -> dict[str, int]:
    return {str(code): count for code, count in sorted(Counter(exit_codes).items())}


def summarize_project_file(
    path: Path,
    *,
    bootstrap_iterations: int,
    output_dir: Path,
    metadata: dict[str, object] | None,
) -> dict[str, dict[str, object]]:
    data = json.loads(path.read_text())
    project: dict[str, dict[str, object]] = {}
    project_name = path.name.removesuffix(".hyperfine.json")
    warmup = metadata_warmup(metadata)

    for index, result in enumerate(data.get("results", [])):
        command = result["command"]
        wall_times = [float(value) for value in result.get("times", [])]
        resource_records = read_resource_records(
            output_dir=output_dir,
            metadata=metadata,
            project_name=project_name,
            command=command,
            warmup=warmup,
            measured_runs=len(wall_times),
        )

        user_s = float(result.get("user", 0.0))
        system_s = float(result.get("system", 0.0))
        if resource_records:
            maxrss_mb = [record["maxrss_kb"] / 1024 for record in resource_records]
            cpu_samples_s = [
                record["user_s"] + record["system_s"] for record in resource_records
            ]
            cpu_s: dict[str, object] = distribution_stats(
                cpu_samples_s,
                bootstrap_iterations=bootstrap_iterations,
                seed=3000 + index,
            )
            cpu_s.update(
                {
                    "user_mean": statistics.fmean(
                        record["user_s"] for record in resource_records
                    ),
                    "system_mean": statistics.fmean(
                        record["system_s"] for record in resource_records
                    ),
                    "source": "GNU time sidecar",
                }
            )
            maxrss_source = "GNU time sidecar"
        else:
            maxrss_mb = [
                float(value) / 1024 / 1024
                for value in result.get("memory_usage_byte", [])
                if value is not None
            ]
            cpu_s = {
                "mean": user_s + system_s,
                "user_mean": user_s,
                "system_mean": system_s,
                "note": "Hyperfine JSON stores mean user/system CPU time, not per-run CPU samples.",
                "source": "hyperfine",
            }
            maxrss_source = "hyperfine"

        project[command] = {
            "wall_s": distribution_stats(
                wall_times,
                bootstrap_iterations=bootstrap_iterations,
                seed=1000 + index,
            ),
            "cpu_s": cpu_s,
            "maxrss_mb": distribution_stats(
                maxrss_mb,
                bootstrap_iterations=bootstrap_iterations,
                seed=2000 + index,
            ),
            "exit_codes": exit_code_summary(
                [int(value) for value in result.get("exit_codes", [])]
            ),
        }
        project[command]["maxrss_mb"]["source"] = maxrss_source

    return project


def metadata_warmup(metadata: dict[str, object] | None) -> int:
    if not metadata:
        return 0

    argv = metadata.get("argv")
    if not isinstance(argv, list):
        return 0

    for index, value in enumerate(argv):
        if value == "--warmup" and index + 1 < len(argv):
            return int(argv[index + 1])
    return 3


def read_resource_records(
    *,
    output_dir: Path,
    metadata: dict[str, object] | None,
    project_name: str,
    command: str,
    warmup: int,
    measured_runs: int,
) -> list[dict[str, float]] | None:
    if not metadata:
        return None

    runs = metadata.get("runs")
    if not isinstance(runs, dict):
        return None

    project_metadata = runs.get(project_name)
    if not isinstance(project_metadata, dict):
        return None

    resource_usage = project_metadata.get("resource_usage")
    if not isinstance(resource_usage, dict):
        return None

    command_resource = resource_usage.get(command)
    if not isinstance(command_resource, dict):
        return None

    relative_path = command_resource.get("path")
    if not isinstance(relative_path, str):
        return None

    resource_path = output_dir / relative_path
    if not resource_path.exists():
        return None

    records: list[dict[str, float]] = []
    for line in resource_path.read_text().splitlines():
        parts = line.split("\t")
        if len(parts) != 5:
            continue
        maxrss_kb, elapsed_s, user_s, system_s, exit_code = parts
        records.append(
            {
                "maxrss_kb": float(maxrss_kb),
                "elapsed_s": float(elapsed_s),
                "user_s": float(user_s),
                "system_s": float(system_s),
                "exit_code": float(exit_code),
            }
        )

    if measured_runs <= 0:
        return records
    if len(records) >= warmup + measured_runs:
        return records[warmup : warmup + measured_runs]
    if len(records) >= measured_runs:
        return records[-measured_runs:]
    return records


def aggregate(projects: dict[str, dict[str, dict[str, object]]]) -> dict[str, object]:
    by_command: dict[str, list[dict[str, object]]] = defaultdict(list)
    for project in projects.values():
        for command, metrics in project.items():
            by_command[command].append(metrics)

    summary: dict[str, object] = {}
    for command, rows in sorted(by_command.items()):
        wall_means = [
            row["wall_s"]["mean"]
            for row in rows
            if isinstance(row.get("wall_s"), dict) and "mean" in row["wall_s"]
        ]
        cpu_means = [
            row["cpu_s"]["mean"]
            for row in rows
            if isinstance(row.get("cpu_s"), dict) and "mean" in row["cpu_s"]
        ]
        maxrss_means = [
            row["maxrss_mb"]["mean"]
            for row in rows
            if isinstance(row.get("maxrss_mb"), dict) and "mean" in row["maxrss_mb"]
        ]

        summary[command] = {
            "projects": len(rows),
            "geomean_wall_s": geomean(float(value) for value in wall_means),
            "geomean_cpu_s": geomean(float(value) for value in cpu_means),
            "geomean_maxrss_mb": geomean(float(value) for value in maxrss_means),
        }

    return summary


def markdown_table(aggregate_summary: dict[str, object]) -> str:
    rows = []
    for command, metrics in sorted(
        aggregate_summary.items(),
        key=lambda item: (
            float(item[1].get("geomean_wall_s") or math.inf),  # type: ignore[union-attr]
            item[0],
        ),
    ):
        assert isinstance(metrics, dict)
        rows.append(
            [
                command,
                str(metrics["projects"]),
                format_number(metrics.get("geomean_wall_s")),
                format_number(metrics.get("geomean_cpu_s")),
                format_number(metrics.get("geomean_maxrss_mb")),
            ]
        )

    headers = [
        "Command",
        "Projects",
        "Geomean wall (s)",
        "Geomean CPU (s)",
        "Geomean max RSS (MiB)",
    ]
    return render_table(headers, rows)


def format_number(value: object) -> str:
    if value is None:
        return "n/a"
    return f"{float(value):.3f}"


def render_table(headers: list[str], rows: list[list[str]]) -> str:
    widths = [
        max(len(row[index]) for row in [headers, *rows])
        for index in range(len(headers))
    ]

    def render_row(row: list[str]) -> str:
        return (
            "| "
            + " | ".join(
                cell.ljust(width) for cell, width in zip(row, widths, strict=True)
            )
            + " |"
        )

    separator = "| " + " | ".join("-" * width for width in widths) + " |"
    return "\n".join(
        [render_row(headers), separator, *(render_row(row) for row in rows)]
    )


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Summarize ty_benchmark Hyperfine JSON outputs."
    )
    parser.add_argument("output_dir", type=Path)
    parser.add_argument(
        "--bootstrap-iterations",
        type=int,
        default=2000,
        help="Bootstrap iterations for wall/maxrss confidence intervals.",
    )
    args = parser.parse_args()

    output_dir = args.output_dir.resolve()
    metadata_path = output_dir / "metadata.json"
    metadata = json.loads(metadata_path.read_text()) if metadata_path.exists() else None
    projects = {}
    skipped: dict[str, str] = {}
    for path in sorted(output_dir.glob("*.hyperfine.json")):
        project_name = path.name.removesuffix(".hyperfine.json")
        if path.stat().st_size == 0:
            skipped[project_name] = "empty hyperfine JSON"
            continue
        try:
            projects[project_name] = summarize_project_file(
                path,
                bootstrap_iterations=args.bootstrap_iterations,
                output_dir=output_dir,
                metadata=metadata,
            )
        except json.JSONDecodeError as error:
            skipped[project_name] = f"invalid hyperfine JSON: {error}"

    aggregate_summary = aggregate(projects)
    summary = {
        "projects": projects,
        "aggregate": aggregate_summary,
        "skipped": skipped,
    }

    (output_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n"
    )
    (output_dir / "summary.md").write_text(markdown_table(aggregate_summary) + "\n")

    print(markdown_table(aggregate_summary))
    if skipped:
        print("", file=sys.stderr)
        print("Skipped project files:", file=sys.stderr)
        for project_name, reason in sorted(skipped.items()):
            print(f"- {project_name}: {reason}", file=sys.stderr)


if __name__ == "__main__":
    main()
