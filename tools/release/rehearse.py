#!/usr/bin/env python3
# Copyright 2026 the Footfall Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

"""Inspect Cargo's archives and exercise them in an external consumer (Python 3.11+)."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", action="store_true", help="verify fresh packages before inspecting and consuming them")
    parser.add_argument("--packages-dir", type=Path, help="archive directory to inspect (or write with --package; default: Cargo's target/package)")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--offline", "--no-deps", "--format-version", "1"], cwd=repo,
    ))
    packages = {p["name"]: p for p in metadata["packages"] if p["publish"] != []}
    versions = {p["version"] for p in packages.values()}
    if len(versions) != 1:
        raise SystemExit(f"Public crate versions disagree: {versions}")
    version = versions.pop()
    target = Path(metadata["target_directory"])
    target.mkdir(parents=True, exist_ok=True)
    archives = (args.packages_dir or target / "package").resolve()
    results = {
        "version": version,
        "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
        "source_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=repo, text=True)),
        "archives": {},
        "checks": [],
    }
    log_path = target / "release-consumer.log"

    if args.package:
        if results["source_dirty"]:
            raise SystemExit("Commit or stash changes before packaging a release candidate")
        # Cargo's temporary registry can retain an extracted crate from an
        # earlier rehearsal with the same version. Its source identity depends
        # on the target path, so each candidate needs a fresh staging directory.
        with tempfile.TemporaryDirectory(prefix="footfall-package-") as package_target:
            command = ["cargo", "package", "--workspace", "--all-features", "--offline", "--target-dir", package_target]
            for package in metadata["packages"]:
                if package["publish"] == []:
                    command += ["--exclude", package["name"]]
            print("Verifying fresh workspace packages", flush=True)
            subprocess.run(command, cwd=repo, check=True)
            archives.mkdir(parents=True, exist_ok=True)
            for name in packages:
                filename = f"{name}-{version}.crate"
                shutil.copy2(Path(package_target) / "package" / filename, archives / filename)

    with tempfile.TemporaryDirectory(prefix="footfall-release-consumer-") as temporary, log_path.open("w") as log:
        stage = Path(temporary)
        extracted = stage / "packages"
        for name, package in packages.items():
            archive = archives / f"{name}-{version}.crate"
            prefix = f"{name}-{version}"
            with tarfile.open(archive) as tar:
                vcs = json.loads(tar.extractfile(f"{prefix}/.cargo_vcs_info.json").read())
                if vcs["git"]["sha1"] != results["source_commit"]:
                    raise SystemExit(f"{archive} belongs to another commit; rerun with --package")
                if not results["source_dirty"] and vcs["git"].get("dirty", False):
                    raise SystemExit(f"{archive} was packaged with uncommitted changes; rerun with --package")
                manifest = tomllib.loads(tar.extractfile(f"{prefix}/Cargo.toml").read().decode())
                if manifest["package"]["version"] != version:
                    raise SystemExit(f"Wrong version in {archive}")
                for required in ["README.md", "LICENSE-APACHE", "LICENSE-MIT"]:
                    tar.getmember(f"{prefix}/{required}")
                for member in tar.getmembers():
                    path = PurePosixPath(member.name)
                    if path.is_absolute() or ".." in path.parts or path.parts[0] != prefix:
                        raise SystemExit(f"Unexpected archive path: {member.name}")
                    if member.isdir():
                        continue
                    if not member.isfile():
                        raise SystemExit(f"Unexpected non-file: {member.name}")
                    content = tar.extractfile(member).read()
                    relative = Path(*path.parts[1:])
                    source = Path(package["manifest_path"]).parent / relative
                    if relative == Path("Cargo.toml.orig"):
                        source = Path(package["manifest_path"])
                    if relative not in [Path("Cargo.toml"), Path("Cargo.lock"), Path(".cargo_vcs_info.json")]:
                        if not source.is_file() or source.read_bytes() != content:
                            raise SystemExit(f"Stale package content: {name}/{relative}")
                    destination = extracted / Path(*path.parts)
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination.write_bytes(content)
            results["archives"][name] = hashlib.sha256(archive.read_bytes()).hexdigest()

        config = stage / "patches.toml"
        config.write_text("[patch.crates-io]\n" + "".join(
            f"{name}.path = {json.dumps(str(extracted / f'{name}-{version}'))}\n" for name in packages
        ))
        consumer = stage / "consumer"
        shutil.copytree(repo / "tools/release/consumer", consumer)
        consumer_manifest = tomllib.loads((consumer / "Cargo.toml").read_text())
        if consumer_manifest["dependencies"]["footfall"]["version"] != f"={version}":
            raise SystemExit("Update the consumer's exact Footfall requirement for this release")

        def cargo(label, command, manifest):
            argv = ["cargo", *command, "--manifest-path", str(manifest), "--offline", "--config", str(config)]
            if command[0] != "metadata":
                argv += ["--target-dir", str(target)]
            print(label, flush=True)
            log.write("\n" + " ".join(argv) + "\n")
            log.flush()
            result = subprocess.run(argv, cwd=stage, stdout=subprocess.PIPE, stderr=log, text=True)
            if result.returncode:
                raise SystemExit(f"{label} failed; see {log_path}")
            results["checks"].append(label)
            return result.stdout

        manifest = consumer / "Cargo.toml"
        suffix = ".exe" if os.name == "nt" else ""
        binary = target / "debug" / f"footfall_release_consumer{suffix}"
        cargo("bare external consumer", ["build"], manifest)
        subprocess.run([str(binary)], cwd=stage, check=True, stdout=log, stderr=log)
        graph = json.loads(cargo("external dependency graph", ["metadata", "--format-version", "1", "--all-features"], manifest))
        for package in graph["packages"]:
            if package["name"] in packages:
                source = Path(package["manifest_path"])
                if package["version"] != version or not source.is_relative_to(extracted):
                    raise SystemExit(f"Consumer escaped packaged sources: {source}")
        cargo("full external consumer", ["build", "--all-features"], manifest)
        capture = stage / "smoke.ffcap"
        subprocess.run([str(binary), str(capture)], cwd=stage, check=True, stdout=log, stderr=log)
        results["checks"].append("capture, frame plan, buffer wait, analysis, and export round-trip")
        cargo("packaged no_std facade", ["check", "--no-default-features", "--target", "x86_64-unknown-none"], extracted / f"footfall-{version}/Cargo.toml")
        cli_manifest = extracted / f"footfall_cli-{version}/Cargo.toml"
        cargo("packaged CLI", ["build"], cli_manifest)
        cli = target / "debug" / f"footfall{suffix}"
        cli_version = subprocess.check_output([str(cli), "--version"], text=True).strip()
        if cli_version != f"footfall {version}":
            raise SystemExit(f"Unexpected CLI version: {cli_version}")
        output = subprocess.check_output([str(cli), "analyze", "pacing", str(capture), "--json"], text=True)
        pacing = json.loads(output)
        if pacing["capture_read"] != {"complete": True, "truncated": False, "skipped_snapshots": 0}:
            raise SystemExit("The packaged CLI lost capture completeness")
        if pacing["frames"][0]["buffer_wait_ns"] != 5_000_000:
            raise SystemExit("The packaged CLI lost the buffer wait")
        results["checks"].append("packaged CLI version and structured pacing")
        points = json.loads(subprocess.check_output([
            str(cli), "analyze", "checkpoints", str(capture), "--json",
        ], text=True))["checkpoints"]
        if len(points) != 2 or not all(point["complete"] for point in points):
            raise SystemExit("The packaged CLI lost the checkpoint boundaries")
        interval = json.loads(subprocess.check_output([
            str(cli), "analyze", "checkpoints", str(capture), "--from", str(points[0]["sequence"]),
            "--to", str(points[1]["sequence"]), "--json",
        ], text=True))["interval"]
        if interval["counters"][0]["change"] != 128:
            raise SystemExit("The packaged CLI lost the counter baseline")
        resources = json.loads(subprocess.check_output([
            str(cli), "analyze", "resources", str(capture), "--json",
        ], text=True))["resources"]
        if resources[0]["creation"]["scope_chain"][0]["name"] != "release consumer work":
            raise SystemExit("The packaged CLI lost the creating scope")
        results["checks"].append("packaged named checkpoints, interval changes, and resource provenance")

    report = target / "release-rehearsal.json"
    report.write_text(json.dumps(results, indent=2) + "\n")
    print(f"Verified {len(packages)} archive contents and external consumption; report: {report}")


if __name__ == "__main__":
    main()
