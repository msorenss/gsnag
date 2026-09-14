#!/usr/bin/env python3
"""Package the native release binary using Debian's own dependency scanner."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def main():
    tools = (
        "dpkg-deb",
        "dpkg-shlibdeps",
        "dpkg",
        "strip",
        "desktop-file-validate",
    )
    for tool in tools:
        if not shutil.which(tool):
            raise SystemExit(
                f"Missing {tool}; install dpkg-dev, binutils and "
                "desktop-file-utils"
            )
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    arch = run("dpkg", "--print-architecture")
    output = ROOT / "target/debian"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="gsnag-deb-") as tmp:
        work = Path(tmp)
        stage = work / "debian/gsnag"

        def install(source, destination, mode=0o644):
            target = stage / destination
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / source, target)
            target.chmod(mode)
            return target

        binary = install("target/release/gsnag", "usr/bin/gsnag", 0o755)
        run("strip", "--strip-unneeded", str(binary))
        desktop = install(
            "packaging/gsnag.desktop",
            "usr/share/applications/gsnag.desktop",
        )
        run("desktop-file-validate", str(desktop))
        install(
            "packaging/gsnag.svg",
            "usr/share/icons/hicolor/scalable/apps/gsnag.svg",
        )
        install("README.md", "usr/share/doc/gsnag/README.md")
        install("packaging/copyright", "usr/share/doc/gsnag/copyright")
        (work / "debian").mkdir(exist_ok=True)
        (work / "debian/control").write_text(
            "Source: gsnag\nSection: graphics\nPriority: optional\n"
            "Maintainer: gsnag contributors <gsnag@localhost>\n"
            "\nPackage: gsnag\nArchitecture: any\n"
            "Description: Wayland screenshot editor\n"
        )
        dependencies = run(
            "dpkg-shlibdeps",
            "-O",
            str(binary.relative_to(work)),
            cwd=work,
        )
        depends = next(
            line.split("=", 1)[1]
            for line in dependencies.splitlines()
            if line.startswith("shlibs:Depends=")
        )
        installed_kib = sum(
            p.stat().st_size for p in stage.rglob("*") if p.is_file()
        )
        installed_size = (installed_kib + 1023) // 1024
        control = stage / "DEBIAN/control"
        control.parent.mkdir()
        control.write_text(
            f"Package: gsnag\nVersion: {version}\nArchitecture: {arch}\n"
            "Maintainer: gsnag contributors <gsnag@localhost>\n"
            "Section: graphics\nPriority: optional\n"
            f"Depends: {depends}\nInstalled-Size: {installed_size}\n"
            "Description: Native Wayland screenshot editor and screen "
            "recorder\n"
            " Capture a region and annotate it with shapes, text and image "
            "effects.\n"
            " Save editable projects or export PNG, JPEG and WebP images.\n"
            " Record H.264 video with optional PipeWire system audio and "
            "microphone.\n"
            " Requires a Wayland compositor with screencopy and layer-shell "
            "support.\n"
        )
        # Directories must remain traversable after package installation.
        stage.chmod(0o755)
        control.chmod(0o644)
        for directory in stage.rglob("*"):
            if directory.is_dir():
                directory.chmod(0o755)
        destination = output / f"gsnag_{version}_{arch}.deb"
        subprocess.run(["dpkg-deb", "--root-owner-group", "-Zxz", "--build",
                        str(stage), str(destination)], check=True)
        print(destination)


if __name__ == "__main__":
    main()
