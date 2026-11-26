#!/usr/bin/env python3
"""Copy the Homebrew media tools and rewrite their non-system library paths."""

from pathlib import Path
import shutil
import subprocess
import sys


def output(*args):
    return subprocess.check_output(args, text=True)


def is_system(name):
    return name.startswith(("/usr/lib/", "/System/Library/"))


def dependencies(binary):
    # otool -L prints the dylib's own install name first. otool -l keeps that
    # LC_ID_DYLIB separate from actual dependencies and also exposes LC_RPATH.
    load_command = ""
    links = []
    rpaths = []
    for line in output("otool", "-l", str(binary)).splitlines():
        fields = line.split()
        if len(fields) < 2:
            continue
        if fields[0] == "cmd":
            load_command = fields[1]
        elif fields[0] == "path" and load_command == "LC_RPATH":
            rpaths.append(line.strip().removeprefix("path ").rsplit(" (offset ", 1)[0])
        elif fields[0] == "name" and load_command in {
            "LC_LOAD_DYLIB", "LC_LOAD_WEAK_DYLIB", "LC_REEXPORT_DYLIB",
            "LC_LOAD_UPWARD_DYLIB", "LC_LAZY_LOAD_DYLIB",
        }:
            links.append(line.strip().removeprefix("name ").rsplit(" (offset ", 1)[0])
    return links, rpaths


def expand(name, library_dir, executable_dir):
    if name.startswith("@loader_path/"):
        return library_dir / name.removeprefix("@loader_path/")
    if name.startswith("@executable_path/"):
        return executable_dir / name.removeprefix("@executable_path/")
    return Path(name)


def bundle_tools(tool_dir, macos, frameworks):
    frameworks.mkdir()
    copied = {}
    basename_sources = {}

    def vendor(source, executable_dir, inherited_rpaths=()):
        source = source.resolve(strict=True)
        if source in copied:
            return copied[source]

        tool = source.name in ("ffmpeg", "ffprobe")
        previous = basename_sources.setdefault(source.name, source)
        if previous != source:
            raise RuntimeError(f"two libraries share {source.name}: {source} and {previous}")
        target = (macos if tool else frameworks) / source.name
        copied[source] = target
        shutil.copyfile(source, target)
        target.chmod(0o755)

        links, own_rpaths = dependencies(source)
        rpaths = [
            *(expand(path, source.parent, executable_dir) for path in own_rpaths),
            *inherited_rpaths,
        ]
        edits = [] if tool else ["-id", f"@rpath/{target.name}"]
        for name in links:
            if is_system(name):
                continue
            if name.startswith("@rpath/"):
                tail = name.removeprefix("@rpath/")
                candidates = (base / tail for base in rpaths)
            else:
                candidates = [expand(name, source.parent, executable_dir)]
            path = next((path for path in candidates if path.is_file()), None)
            if path is None:
                raise RuntimeError(f"unresolved dependency {name} of {source}")
            library = vendor(path, executable_dir, rpaths)
            # Both the executable and every copied dylib use this fixed directory.
            edits += ["-change", name, f"@executable_path/../Frameworks/{library.name}"]
        for path in dict.fromkeys(own_rpaths):
            edits += ["-delete_rpath", path]
        if edits:
            subprocess.run(["install_name_tool", *edits, str(target)], check=True)
        return target

    for name in ("ffmpeg", "ffprobe"):
        source = (tool_dir / name).resolve(strict=True)
        vendor(source, source.parent)


if __name__ == "__main__":
    bundle_tools(*(Path(arg).resolve() for arg in sys.argv[1:]))
