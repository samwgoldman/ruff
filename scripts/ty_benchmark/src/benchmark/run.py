from __future__ import annotations

import argparse
import json
import logging
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import Final

from benchmark import Command, Hyperfine
from benchmark.projects import ALL as ALL_PROJECTS
from benchmark.projects import IncrementalEdit, Project
from benchmark.snapshot import SnapshotRunner
from benchmark.tool import Mypy, Pyrefly, Pyright, Tool, Ty
from benchmark.venv import Venv

TOOL_CHOICES: Final = ["ty", "pyrefly", "mypy", "pyright"]
RESOURCE_TIME_FORMAT: Final = "%M\t%e\t%U\t%S\t%x"


def parse_labeled_path(value: str, default_label: str) -> tuple[str, Path]:
    if "=" in value:
        label, path = value.split("=", 1)
    else:
        label, path = default_label, value
    return label, Path(path)


def capture(command: list[str], *, cwd: Path | None = None) -> dict[str, object]:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            timeout=20,
        )
    except Exception as error:
        return {"command": command, "error": repr(error)}

    return {
        "command": command,
        "returncode": result.returncode,
        "stdout": result.stdout.strip(),
        "stderr": result.stderr.strip(),
    }


def git_output(args: list[str], *, cwd: Path) -> str | None:
    result = capture(["git", *args], cwd=cwd)
    if result.get("returncode") == 0:
        return str(result.get("stdout", ""))
    return None


def find_git_root(path: Path) -> Path | None:
    for parent in [path, *path.parents]:
        if (parent / ".git").exists():
            return parent
    return None


def edit_to_json(edit: IncrementalEdit | None) -> dict[str, object] | None:
    if edit is None:
        return None
    return {
        "edited_file": edit.edited_file,
        "affected_files": edit.affected_files,
        "replace_text": edit.replace_text,
        "replacement": edit.replacement,
    }


def project_to_json(project: Project) -> dict[str, object]:
    return {
        "name": project.name,
        "repository": project.repository,
        "revision": project.revision,
        "python_version": project.python_version,
        "install_arguments": project.install_arguments,
        "skip": project.skip,
        "include": project.include,
        "exclude": project.exclude,
        "edit": edit_to_json(project.edit),
        "file_replacements": [
            {"path": path, "old_text": old, "new_text": new}
            for path, old, new in project.file_replacements
        ],
    }


def command_version(command: list[str], name: str, *, cwd: Path) -> dict[str, object]:
    executable = command[0]
    lower_name = name.lower()

    candidates: list[list[str]]
    if "pyrefly" in lower_name:
        candidates = [[executable, "--version"], [executable, "version"]]
    elif "pyright" in lower_name:
        candidates = [[executable, "--version"]]
    elif "mypy" in lower_name:
        candidates = [[executable, "--version"]]
    elif "ty" in lower_name:
        candidates = [[executable, "version"], [executable, "--version"]]
    else:
        candidates = [[executable, "--version"]]

    attempts = [capture(candidate, cwd=cwd) for candidate in candidates]
    for attempt in attempts:
        if attempt.get("returncode") == 0:
            return attempt
    return attempts[0]


def write_metadata(output_dir: Path, metadata: dict[str, object]) -> None:
    (output_dir / "metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    )


def safe_filename(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", value).strip("_") or "command"


def is_gnu_time(path: Path) -> bool:
    result = capture([str(path), "--version"])
    output = f"{result.get('stdout', '')}\n{result.get('stderr', '')}"
    return result.get("returncode") == 0 and "GNU" in output


def find_gnu_time() -> Path:
    candidates = []
    if gtime := shutil.which("gtime"):
        candidates.append(Path(gtime))
    if time := shutil.which("time"):
        candidates.append(Path(time))
    candidates.append(Path("/usr/bin/time"))

    seen: set[Path] = set()
    for candidate in candidates:
        candidate = candidate.resolve()
        if candidate in seen or not candidate.exists():
            continue
        seen.add(candidate)
        if is_gnu_time(candidate):
            return candidate

    raise RuntimeError(
        "GNU time is required to record max RSS. Install gnu-time as `gtime` "
        "on macOS, or ensure `/usr/bin/time` is GNU time."
    )


def wrap_resource_usage(
    *,
    commands: list[Command],
    output_dir: Path,
    project_name: str,
) -> tuple[list[Command], dict[str, object]]:
    time_path = find_gnu_time()
    resource_dir = output_dir / "resource-usage" / project_name
    resource_dir.mkdir(parents=True, exist_ok=True)

    wrapped: list[Command] = []
    manifest: dict[str, object] = {}
    for index, command in enumerate(commands):
        resource_file = resource_dir / f"{index:02d}-{safe_filename(command.name)}.tsv"
        resource_file.write_text("")
        wrapped_command = [
            str(time_path),
            "--quiet",
            "-f",
            RESOURCE_TIME_FORMAT,
            "-a",
            "-o",
            str(resource_file),
            "--",
            *command.command,
        ]
        wrapped.append(
            Command(
                name=command.name,
                command=wrapped_command,
                prepare=command.prepare,
            )
        )
        manifest[command.name] = {
            "path": str(resource_file.relative_to(output_dir)),
            "format": "maxrss_kb\\telapsed_s\\tuser_s\\tsystem_s\\texit_code",
            "source": str(time_path),
        }

    return wrapped, manifest


def main() -> None:
    """Run the benchmark."""
    parser = argparse.ArgumentParser(
        description="Benchmark ty against other packaging tools."
    )
    parser.add_argument(
        "--verbose", "-v", action="store_true", help="Print verbose output."
    )
    parser.add_argument(
        "--warmup",
        type=int,
        help="The number of warmup runs to perform.",
        default=3,
    )
    parser.add_argument(
        "--min-runs",
        type=int,
        help="The minimum number of runs to perform.",
        default=10,
    )
    parser.add_argument(
        "--project",
        "-p",
        type=str,
        help="The project(s) to run.",
        choices=[project.name for project in ALL_PROJECTS],
        action="append",
    )
    parser.add_argument(
        "--tool",
        help="Which tool to benchmark.",
        choices=TOOL_CHOICES,
        action="append",
    )

    parser.add_argument(
        "--ty-path",
        action="append",
        type=str,
        help="Path to the ty binary to benchmark. Use label=/path/to/ty to control the command name.",
    )
    parser.add_argument(
        "--pyrefly-path",
        action="append",
        type=str,
        help="Path to a Pyrefly binary to benchmark. Use label=/path/to/pyrefly to control the command name.",
    )
    parser.add_argument(
        "--pyright-path",
        type=Path,
        help="Path to the Pyright binary to benchmark. The matching pyright-langserver is expected beside it.",
    )
    parser.add_argument(
        "--pyright-workers",
        action="append",
        type=int,
        help="Explicit Pyright --threads count. Can be repeated to benchmark multiple thread counts. If omitted, Pyright chooses its own threaded default.",
    )
    parser.add_argument(
        "--mypy-workers",
        action="append",
        type=int,
        help="Explicit mypy --num-workers value. Can be repeated to benchmark multiple worker counts.",
    )
    parser.add_argument(
        "--mypy-path",
        type=Path,
        help="Path to the mypy binary to benchmark.",
    )
    parser.add_argument(
        "--mypy-requirement",
        default="mypy==2.1.0",
        help="Requirement to install into each project venv for the mypy executable and plugin loading. Ignored when --mypy-path is provided.",
    )

    parser.add_argument(
        "--single-threaded",
        action="store_true",
        help="Run the type checkers single threaded",
    )
    parser.add_argument(
        "--max-workers",
        type=int,
        help="Cap worker count for tools that expose a worker setting. Sets TY_MAX_PARALLELISM and passes --threads to Pyrefly.",
    )

    parser.add_argument(
        "--warm",
        action=argparse.BooleanOptionalAction,
        help="Run warm benchmarks in addition to cold benchmarks (for tools supporting it)",
    )

    parser.add_argument(
        "--snapshot",
        action="store_true",
        help="Run commands and snapshot their output instead of benchmarking with hyperfine.",
    )

    parser.add_argument(
        "--accept",
        action="store_true",
        help="Accept snapshot changes (only valid with --snapshot).",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="Directory for hyperfine JSON, generated configs, and reproducibility metadata.",
    )

    args = parser.parse_args()
    logging.basicConfig(
        level=logging.INFO if args.verbose else logging.WARN,
        format="%(asctime)s %(levelname)s %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )

    # Validate arguments.
    if args.accept and not args.snapshot:
        parser.error("--accept can only be used with --snapshot")

    if args.snapshot and args.warm:
        parser.error("--warm cannot be used with --snapshot")

    verbose = args.verbose
    warmup = args.warmup
    min_runs = args.min_runs
    output_dir: Path | None = args.output_dir.resolve() if args.output_dir else None

    if output_dir:
        output_dir.mkdir(parents=True, exist_ok=True)

    # Determine the tools to benchmark, based on the user-provided arguments.
    suites: list[Tool] = []

    for tool_name in args.tool or TOOL_CHOICES:
        match tool_name:
            case "ty":
                if args.ty_path:
                    for value in args.ty_path:
                        label, path = parse_labeled_path(value, "ty")
                        suites.append(Ty(path=path, label=label))
                else:
                    suites.append(Ty())
            case "pyrefly":
                if args.pyrefly_path:
                    for value in args.pyrefly_path:
                        label, path = parse_labeled_path(value, "Pyrefly")
                        suites.append(Pyrefly(path=Path(path), label=label))
                else:
                    suites.append(Pyrefly())
            case "pyright":
                pyright_workers = args.pyright_workers or [None]
                for workers in pyright_workers:
                    suites.append(Pyright(path=args.pyright_path, workers=workers))
            case "mypy":
                mypy_workers = args.mypy_workers or [None]
                for workers in mypy_workers:
                    suites.append(
                        Mypy(warm=False, path=args.mypy_path, workers=workers)
                    )
                    if args.warm:
                        suites.append(
                            Mypy(warm=True, path=args.mypy_path, workers=workers)
                        )
            case _:
                raise ValueError(f"Unknown tool: {tool_name}")

    projects = (
        [project for project in ALL_PROJECTS if project.name in args.project]
        if args.project
        else ALL_PROJECTS
    )

    metadata: dict[str, object] | None = None
    if output_dir:
        benchmark_root = find_git_root(Path(__file__).resolve())
        metadata = {
            "created_at": datetime.now(UTC).isoformat(),
            "argv": sys.argv,
            "platform": {
                "platform": platform.platform(),
                "python": sys.version,
            },
            "benchmark_script": {
                "git_root": str(benchmark_root) if benchmark_root else None,
                "git_revision": git_output(["rev-parse", "HEAD"], cwd=benchmark_root)
                if benchmark_root
                else None,
                "git_status": git_output(["status", "--short"], cwd=benchmark_root)
                if benchmark_root
                else None,
            },
            "tools_requested": [suite.name() for suite in suites],
            "projects": [project_to_json(project) for project in projects],
            "worker_policy": {
                "single_threaded": args.single_threaded,
                "max_workers": args.max_workers,
                "mypy_workers": args.mypy_workers,
                "mypy_requirement": args.mypy_requirement,
                "mypy_path": str(args.mypy_path) if args.mypy_path else None,
                "pyright_workers": args.pyright_workers,
            },
            "environment": {
                "uv": capture(["uv", "--version"]),
                "hyperfine": capture(["hyperfine", "--version"]),
                "node": capture(["node", "--version"]),
            },
            "runs": {},
        }
        if benchmark_root:
            diff = git_output(
                [
                    "diff",
                    "--",
                    "scripts/ty_benchmark/src/benchmark",
                    "scripts/ty_benchmark/pyproject.toml",
                    "scripts/ty_benchmark/package.json",
                    "scripts/ty_benchmark/package-lock.json",
                    "scripts/ty_benchmark/uv.lock",
                ],
                cwd=benchmark_root,
            )
            if diff is not None:
                (output_dir / "benchmark_script.diff").write_text(diff)
                metadata["benchmark_script"]["diff_file"] = "benchmark_script.diff"  # type: ignore[index]
        write_metadata(output_dir, metadata)

    benchmark_env = os.environ.copy()

    if args.single_threaded:
        benchmark_env["TY_MAX_PARALLELISM"] = "1"
        benchmark_env["PYREFLY_THREADS"] = "1"
    elif args.max_workers is not None:
        benchmark_env["TY_MAX_PARALLELISM"] = str(args.max_workers)
        benchmark_env["PYREFLY_THREADS"] = str(args.max_workers)

    first = True

    for project in projects:
        if skip_reason := project.skip:
            print(f"Skipping {project.name}: {skip_reason}")
            continue

        with tempfile.TemporaryDirectory() as tempdir:
            cwd = Path(tempdir)
            project.clone(cwd)
            project.prepare_checkout(cwd)

            venv = Venv.create(
                project=project.name, parent=cwd, python_version=project.python_version
            )
            venv.install(
                project.install_arguments,
                mypy_requirement=None if args.mypy_path else args.mypy_requirement,
            )

            commands = []

            for suite in suites:
                suite.write_config(project, venv)
                commands.append(
                    suite.command(
                        project,
                        venv,
                        args.single_threaded,
                        args.max_workers,
                    )
                )

            if not commands:
                continue

            benchmark_commands = commands
            resource_usage: dict[str, object] = {}
            if output_dir and not args.snapshot:
                benchmark_commands, resource_usage = wrap_resource_usage(
                    commands=commands,
                    output_dir=output_dir,
                    project_name=project.name,
                )

            hyperfine_json: Path | bool = False
            if output_dir:
                hyperfine_json = output_dir / f"{project.name}.hyperfine.json"
                config_dir = output_dir / "configs" / project.name
                config_dir.mkdir(parents=True, exist_ok=True)
                for config_name in ("ty.toml", "pyrefly.toml", "pyrightconfig.json"):
                    config_path = cwd / config_name
                    if config_path.exists():
                        shutil.copy2(config_path, config_dir / config_name)

                run_metadata = {
                    "project": project_to_json(project),
                    "resolved_revision": git_output(["rev-parse", "HEAD"], cwd=cwd),
                    "venv": {
                        "path": str(venv.path),
                        "python": str(venv.python),
                        "python_version": capture(
                            [str(venv.python), "--version"], cwd=cwd
                        ),
                    },
                    "commands": {command.name: command.command for command in commands},
                    "benchmark_commands": {
                        command.name: command.command for command in benchmark_commands
                    },
                    "tool_versions": {
                        command.name: command_version(
                            command.command, command.name, cwd=cwd
                        )
                        for command in commands
                    },
                    "resource_usage": resource_usage,
                    "generated_configs": {
                        config_name: f"configs/{project.name}/{config_name}"
                        for config_name in (
                            "ty.toml",
                            "pyrefly.toml",
                            "pyrightconfig.json",
                        )
                        if (config_dir / config_name).exists()
                    },
                    "hyperfine_json": hyperfine_json.name,
                }
                assert metadata is not None
                metadata["runs"][project.name] = run_metadata  # type: ignore[index]
                write_metadata(output_dir, metadata)

            if not first:
                print("")
                print(
                    "-------------------------------------------------------------------------------"
                )
                print("")

            print(f"{project.name}")
            print("-" * len(project.name))
            print("")

            if args.snapshot:
                # Get the directory where run.py is located to find snapshots directory.
                script_dir = Path(__file__).parent.parent.parent
                snapshot_dir = script_dir / "snapshots"

                snapshot_runner = SnapshotRunner(
                    name=f"{project.name}",
                    commands=commands,
                    snapshot_dir=snapshot_dir,
                    accept=args.accept,
                )
                snapshot_runner.run(cwd=cwd, env=benchmark_env)
                hyperfine_returncode = None
            else:
                hyperfine = Hyperfine(
                    name=f"{project.name}",
                    commands=benchmark_commands,
                    warmup=warmup,
                    min_runs=min_runs,
                    verbose=verbose,
                    json=hyperfine_json,
                )
                hyperfine_returncode = hyperfine.run(cwd=cwd, env=benchmark_env)

            if output_dir and metadata is not None:
                metadata["runs"][project.name]["hyperfine_returncode"] = (
                    hyperfine_returncode  # type: ignore[index]
                )
                metadata["runs"][project.name]["completed"] = hyperfine_returncode in (
                    0,
                    None,
                )  # type: ignore[index]
                write_metadata(output_dir, metadata)

            first = False
