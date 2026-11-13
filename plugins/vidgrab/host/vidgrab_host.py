#!/usr/bin/env python3
"""VidGrab native messaging host.

Bridges the VidGrab plugin (Wisp) or add-on (LibreWolf / Firefox) to yt-dlp.
The browser launches this script and talks to it over stdin/stdout using the
WebExtension native messaging protocol (4-byte little-endian length prefix +
UTF-8 JSON).
"""

import configparser
import hashlib
import json
import os
import re
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

HOST_VERSION = "1.0.0"
DATA_DIR = os.path.expanduser("~/.local/share/vidgrab")

# Browsers launched from a desktop session often have a minimal PATH, so look
# in the usual user install locations too.
EXTRA_PATHS = [
    DATA_DIR,
    os.path.expanduser("~/.local/bin"),
    "/usr/local/bin",
    "/usr/bin",
    "/snap/bin",
]

PRESETS = {
    "best": ["-f", "bv*+ba/b", "-S", "res,ext:mp4:m4a", "--merge-output-format", "mp4"],
    "2160": ["-f", "bv*+ba/b", "-S", "res:2160,ext:mp4:m4a", "--merge-output-format", "mp4"],
    "1440": ["-f", "bv*+ba/b", "-S", "res:1440,ext:mp4:m4a", "--merge-output-format", "mp4"],
    "1080": ["-f", "bv*+ba/b", "-S", "res:1080,ext:mp4:m4a", "--merge-output-format", "mp4"],
    "720": ["-f", "bv*+ba/b", "-S", "res:720,ext:mp4:m4a", "--merge-output-format", "mp4"],
    "480": ["-f", "bv*+ba/b", "-S", "res:480,ext:mp4:m4a", "--merge-output-format", "mp4"],
    "audio_mp3": ["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0"],
    "audio_m4a": ["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a"],
    "audio_best": ["-f", "ba/b", "-x"],
}

# Linux allows 255 bytes per file name, and yt-dlp appends suffixes such as
# ".f137.mp4.part", so cap both the title and the ID (which on generic pages
# can be a long chunk of the URL).
DEFAULT_TEMPLATE = "%(title).150B [%(id).30B].%(ext)s"
OLD_DEFAULT_TEMPLATE = "%(title).200B [%(id)s].%(ext)s"
SHORT_TEMPLATE = "%(title).60B.%(ext)s"

write_lock = threading.Lock()
jobs = {}  # job id -> subprocess.Popen
jobs_lock = threading.Lock()


def send(msg):
    data = json.dumps(msg).encode("utf-8")
    with write_lock:
        sys.stdout.buffer.write(struct.pack("<I", len(data)))
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()


def read_message():
    raw_len = sys.stdin.buffer.read(4)
    if len(raw_len) < 4:
        return None
    (length,) = struct.unpack("<I", raw_len)
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))


def is_ytdlp_name(path):
    return re.fullmatch(r"yt-dlp(_linux(_aarch64|_armv7l)?|_macos)?", os.path.basename(path)) is not None


def find_ytdlp(custom=None):
    if custom:
        custom = os.path.realpath(os.path.expanduser(custom))
        # Only a yt-dlp program: the setting must not run anything else.
        if is_ytdlp_name(custom) and os.path.isfile(custom) and os.access(custom, os.X_OK):
            return [custom]
    found = shutil.which("yt-dlp", path=os.pathsep.join(EXTRA_PATHS + [os.environ.get("PATH", "")]))
    if found:
        return [found]
    # Fall back to a pip-installed module.
    try:
        import yt_dlp  # noqa: F401
        return [sys.executable, "-m", "yt_dlp"]
    except ImportError:
        return None


def search_path():
    return os.pathsep.join(EXTRA_PATHS + [os.environ.get("PATH", "")])


def find_js_runtime():
    """YouTube needs a JS runtime to solve its player challenges. yt-dlp only
    enables deno by default, so name whichever one is installed explicitly."""
    for name in ("deno", "node", "bun", "qjs"):
        path = shutil.which(name, path=search_path())
        if path:
            return ("quickjs" if name == "qjs" else name), path
    return None


# Where LibreWolf keeps profiles.ini (newest layout first).
PROFILE_ROOTS = [
    "~/.config/librewolf/librewolf",
    "~/.librewolf",
    "~/.var/app/io.gitlab.librewolf-community/.librewolf",
    "~/.config/mozilla/firefox",
    "~/.mozilla/firefox",
]


def running_profile():
    """The profile of the browser that launched us: it keeps cookies.sqlite open."""
    pid = os.getppid()
    for _ in range(4):
        if pid <= 1:
            break
        try:
            for fd in os.listdir(f"/proc/{pid}/fd"):
                try:
                    target = os.readlink(f"/proc/{pid}/fd/{fd}")
                except OSError:
                    continue
                if target.endswith("/cookies.sqlite"):
                    return os.path.dirname(target)
            with open(f"/proc/{pid}/stat") as f:
                pid = int(f.read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            break
    return None


def default_profile():
    for root in PROFILE_ROOTS:
        root = os.path.expanduser(root)
        ini = configparser.ConfigParser()
        if not ini.read(os.path.join(root, "profiles.ini")):
            continue
        paths = [ini[s]["Default"] for s in ini.sections() if s.startswith("Install") and "Default" in ini[s]]
        for sec in ini.sections():
            if sec.startswith("Profile") and ini[sec].get("Default") == "1":
                paths.append(ini[sec]["Path"])
        paths += [ini[s]["Path"] for s in ini.sections() if s.startswith("Profile") and "Path" in ini[s]]
        for path in paths:
            full = path if os.path.isabs(path) else os.path.join(root, path)
            if os.path.isfile(os.path.join(full, "cookies.sqlite")):
                return full
    return None


def browser_profile():
    return running_profile() or default_profile()


def cookie_args(opts):
    # Cookies the browser handed over (Wisp keeps its cookies in memory).
    if opts.get("cookiesFile"):
        return ["--cookies", opts["cookiesFile"]]
    manual = (opts.get("cookiesFromBrowser") or "").strip()
    if manual:
        return ["--cookies-from-browser", os.path.expanduser(manual)]
    profile = browser_profile()
    return ["--cookies-from-browser", f"firefox:{profile}"] if profile else []


def env_with_paths():
    env = os.environ.copy()
    env["PATH"] = os.pathsep.join(EXTRA_PATHS + [env.get("PATH", "")])
    env["PYTHONUNBUFFERED"] = "1"
    return env


def run_capture(cmd, timeout=60):
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env_with_paths())
    return p.returncode, p.stdout.strip(), p.stderr.strip()


# yt-dlp fetched by VidGrab itself, and when it last checked for a newer one.
OWN_YTDLP = os.path.join(DATA_DIR, "yt-dlp")
UPDATE_STAMP = os.path.join(DATA_DIR, "last-update-check")
UPDATE_EVERY = 7 * 24 * 3600
install_lock = threading.Lock()

# Where yt-dlp's releases are, and the key they're signed with: each release
# has SHA2-256SUMS (the hash of every file in it) and SHA2-256SUMS.sig, that
# list signed with yt-dlp's release key. VidGrab runs a yt-dlp it fetched
# only if its hash is in a list that key signed, so a download that was
# swapped or damaged on the way is never run.
#
# The key is the RSA key in public.key of yt-dlp's repository (fingerprint
# AC0C BBE6 848D 6A87 3464 AF4E 57CF 6593 3B5A 7581). If yt-dlp ever signs
# with another one, fetching stops working until the new key is put here.
YTDLP_RELEASE = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/"
YTDLP_KEY_E = 65537
YTDLP_KEY_N = int(
    "f4ac5f738c63c0b74b6196de42d5e6f371c0155fb35bd6ff9ea908e4f9611975"
    "31135428375803399e002b947766fdf66acd480efa9018ac1eb418a7a0edf7d4"
    "0dff92d1714a11975e44b61e01aaaa2a14a26d56a50e3bce04ecedbb75bad3b1"
    "cb113400a80ef04cfb88b765aa9a92fad0d21ccabdbb2f1aa681d37798dbd9da"
    "22cef9b3f49ca47f0be3e013d1f8cadcb00dad76cccc7a3e66429d538e3aee97"
    "644166754c71750f5f143a9df4b701298d8b0ce712628d6c698d185a26561edc"
    "d5a19bb590ece39d2e458eaff204724b2260c7ee93711e6eecfcf9dababa6640"
    "5b4316bba4b7eda58439e9294986aff23fdc81ea1ff5fef673e84b8ba2a90e47"
    "a5b941f012bee167e96a2120ef92e7bee38f7d920f298c976bfd00ed0a233308"
    "0eac3f3ca334fe54290a92427e8a35d9ee01cd8e670169474a2bb7c320439823"
    "adcbc9ce905a41904e28057df4945d61c308ef5a5cd84fdde8ea15680632d094"
    "b2b6e4709916b9663bab9011cbfd391db48cdcae0353d7aab67c66895d5643c9"
    "d7b88f49809411be1e4f4e49f2e4bb71ab3726aaf58ef538c705baecd7804f10"
    "ef5f717c4a2ff08505bcd70d2bef02002b76aaa53289dc43fca271395f2faab3"
    "27c10a162a96d7840c4dd027e92bae653a1d5c24e2227c284fb5a38ee14f28a7"
    "6f7c7d463f86e7a9dcd39f32f10920c812d7f27cfb085fef4623b26493fcd98d",
    16,
)
# When the newest signed list seen was signed, so an older release can't be
# passed off as the latest.
SIGNED_STAMP = os.path.join(DATA_DIR, "yt-dlp-signed")
# The hashes OpenPGP signatures here may use, by OpenPGP number, with the
# bytes that go before the hash in an RSA signature.
SIGNATURE_HASHES = {
    8: (hashlib.sha256, bytes.fromhex("3031300d060960864801650304020105000420")),
    10: (hashlib.sha512, bytes.fromhex("3051300d060960864801650304020305000440")),
}


def verify_signature(data, sig):
    """Check `sig`, a detached OpenPGP signature (as in SHA2-256SUMS.sig), on
    `data` against yt-dlp's release key. Returns when it was signed (Unix
    time); raises ValueError unless the signature is good."""
    try:
        first = sig[0]
        if first & 0xC0 == 0xC0:  # new packet format
            tag = first & 0x3F
            if sig[1] < 192:
                length, start = sig[1], 2
            elif sig[1] < 224:
                length, start = ((sig[1] - 192) << 8) + sig[2] + 192, 3
            elif sig[1] == 255:
                length, start = int.from_bytes(sig[2:6], "big"), 6
            else:
                raise ValueError("unsupported signature packet")
        elif first & 0xC0 == 0x80 and first & 3 < 3:  # old packet format
            tag = (first >> 2) & 0xF
            size = 1 << (first & 3)
            length, start = int.from_bytes(sig[1:1 + size], "big"), 1 + size
        else:
            raise ValueError("not an OpenPGP signature")
        body = sig[start:]
        # One signature packet and nothing else: version 4, over binary
        # data, made with RSA.
        if tag != 2 or length != len(body) or body[0] != 4 or body[1] != 0 or body[2] != 1:
            raise ValueError("not a signature of the kind yt-dlp makes")
        new_hash, prefix = SIGNATURE_HASHES[body[3]]
        hashed_end = 6 + int.from_bytes(body[4:6], "big")
        unhashed_end = hashed_end + 2 + int.from_bytes(body[hashed_end:hashed_end + 2], "big")
        bits = int.from_bytes(body[unhashed_end + 2:unhashed_end + 4], "big")
        value = body[unhashed_end + 4:]
        if unhashed_end + 4 > len(body) or len(value) != (bits + 7) // 8:
            raise ValueError("damaged signature")
        signed = body[:hashed_end]
        digest = new_hash(data + signed + b"\x04\xff" + len(signed).to_bytes(4, "big")).digest()
        if digest[:2] != body[unhashed_end:unhashed_end + 2]:
            raise ValueError("the signature isn't for this file")
        # RSA (PKCS #1 v1.5): the signature, raised to the key's exponent,
        # must be exactly the padded hash.
        size = (YTDLP_KEY_N.bit_length() + 7) // 8
        padded = b"\x00\x01" + b"\xff" * (size - 3 - len(prefix) - len(digest)) + b"\x00" + prefix + digest
        number = int.from_bytes(value, "big")
        if number >= YTDLP_KEY_N or pow(number, YTDLP_KEY_E, YTDLP_KEY_N) != int.from_bytes(padded, "big"):
            raise ValueError("the signature isn't yt-dlp's")
        # The time it was made is among the signed fields (subpacket 2).
        i = 6
        while i < hashed_end:
            n = signed[i]
            if n >= 192:
                raise ValueError("unsupported signature packet")
            if n == 5 and signed[i + 1] & 0x7F == 2:
                return int.from_bytes(signed[i + 2:i + 6], "big")
            i += 1 + n
        raise ValueError("the signature has no date")
    except (IndexError, KeyError):
        raise ValueError("damaged signature") from None


def listed_sha256(sums, name):
    """The SHA-256 a SHA2-256SUMS list gives for the file `name`."""
    for line in sums.decode("utf-8", "replace").splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == name and re.fullmatch(r"[0-9a-f]{64}", parts[0]):
            return parts[0]
    raise ValueError(f"{name} isn't in the list of hashes")


def fetch(url, limit):
    """What's at `url`, if it's at most `limit` bytes."""
    with urllib.request.urlopen(url, timeout=120) as r:
        data = r.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"{url} is larger than expected")
    return data


def last_signed():
    try:
        with open(SIGNED_STAMP) as f:
            return int(f.read().strip())
    except (OSError, ValueError):
        return 0


def file_sha256(path):
    h = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
    except OSError:
        return None
    return h.hexdigest()


def handle_status(msg):
    ytdlp = find_ytdlp(msg.get("ytdlpPath"))
    if not ytdlp and msg.get("autoInstall"):
        # First use: fetch yt-dlp in the background, then report again.
        send({"type": "status", "hostVersion": HOST_VERSION, "installing": True})
        threading.Thread(target=install_then_status, args=(msg,), daemon=True).start()
        return
    if ytdlp == [OWN_YTDLP]:
        threading.Thread(target=update_if_due, daemon=True).start()
    info = {
        "type": "status",
        "hostVersion": HOST_VERSION,
        "ytdlp": None,
        "ytdlpPath": None,
        "ffmpeg": shutil.which("ffmpeg", path=search_path()),
        "jsRuntime": (find_js_runtime() or (None, None))[1],
        "profile": browser_profile(),
        "defaultDir": default_download_dir(),
    }
    if ytdlp:
        info["ytdlpPath"] = " ".join(ytdlp)
        try:
            code, out, _ = run_capture(ytdlp + ["--version"], timeout=30)
            if code == 0:
                info["ytdlp"] = out
        except Exception as e:  # noqa: BLE001
            info["error"] = str(e)
    send(info)


def handle_update(msg):
    ytdlp = find_ytdlp(msg.get("ytdlpPath"))
    if not ytdlp:
        send({"type": "update", "ok": False, "output": "yt-dlp not found"})
        return
    if ytdlp == [OWN_YTDLP]:
        # The one VidGrab fetched: replaced the way it was fetched, checked
        # against yt-dlp's signature.
        ok, output = install_ytdlp(update=True)
        send({"type": "update", "ok": ok, "output": output})
        return
    if ytdlp[0] == sys.executable:
        cmd = [sys.executable, "-m", "pip", "install", "--user", "-U", "yt-dlp"]
    else:
        cmd = ytdlp + ["-U"]
    try:
        code, out, err = run_capture(cmd, timeout=300)
        send({"type": "update", "ok": code == 0, "output": (out + "\n" + err).strip()})
    except Exception as e:  # noqa: BLE001
        send({"type": "update", "ok": False, "output": str(e)})


def default_download_dir():
    try:
        out = subprocess.run(["xdg-user-dir", "DOWNLOAD"], capture_output=True, text=True, timeout=5).stdout.strip()
        if out and os.path.isdir(out):
            return out
    except Exception:  # noqa: BLE001
        pass
    return os.path.expanduser("~/Downloads")


def page_title_template(title, out_dir):
    """Output template that names the file after the page title (used for raw
    streams, which have no title of their own)."""
    name = re.sub(r"[/\x00-\x1f]", " ", title)
    name = re.sub(r"\s+", " ", name).strip(" .") or "video"
    name = name.encode("utf-8")[:150].decode("utf-8", "ignore").strip()
    # Don't let a second stream from the same page overwrite (or be skipped
    # as "already downloaded" because of) the first.
    taken = {os.path.splitext(f)[0] for f in os.listdir(out_dir)} if os.path.isdir(out_dir) else set()
    unique, n = name, 2
    while unique in taken:
        unique, n = f"{name} ({n})", n + 1
    return unique.replace("%", "%%") + ".%(ext)s"


# The yt-dlp options "Extra yt-dlp arguments" may use, and how many values
# each takes. Anything else is refused: options such as --exec,
# --downloader, --postprocessor-args, --config-locations or --plugin-dirs
# would let a settings field run any program, and -o, -P, --cookies or
# --batch-file would read or write files outside the download folder.
EXTRA_ALLOWED = {
    # Speed and retries
    "-r": 1, "--limit-rate": 1, "--throttled-rate": 1, "-R": 1, "--retries": 1,
    "--fragment-retries": 1, "--extractor-retries": 1, "-N": 1, "--concurrent-fragments": 1,
    "--http-chunk-size": 1, "--sleep-interval": 1, "--min-sleep-interval": 1,
    "--max-sleep-interval": 1, "--sleep-requests": 1, "--sleep-subtitles": 1,
    "--socket-timeout": 1, "-4": 0, "--force-ipv4": 0, "-6": 0, "--force-ipv6": 0,
    "--geo-bypass": 0, "--no-geo-bypass": 0, "--xff": 1,
    # Formats
    "-f": 1, "--format": 1, "-S": 1, "--format-sort": 1, "--format-sort-force": 0,
    "--merge-output-format": 1, "--remux-video": 1, "--recode-video": 1,
    "--prefer-free-formats": 0, "--check-formats": 0, "--no-check-formats": 0,
    "-x": 0, "--extract-audio": 0, "--audio-format": 1, "--audio-quality": 1, "-k": 0, "--keep-video": 0,
    "--video-multistreams": 0, "--audio-multistreams": 0,
    # Subtitles, metadata, chapters
    "--write-subs": 0, "--write-auto-subs": 0, "--sub-langs": 1, "--sub-format": 1, "--embed-subs": 0,
    "--convert-subs": 1, "--embed-thumbnail": 0, "--embed-metadata": 0, "--embed-chapters": 0,
    "--no-embed-chapters": 0, "--embed-info-json": 0, "--sponsorblock-remove": 1, "--sponsorblock-mark": 1,
    # Files
    "--no-mtime": 0, "--no-part": 0, "--restrict-filenames": 0, "--windows-filenames": 0,
    "--trim-filenames": 1, "-w": 0, "--no-overwrites": 0, "--force-overwrites": 0,
    "--min-filesize": 1, "--max-filesize": 1,
    # Live streams and playlists
    "--live-from-start": 0, "--no-live-from-start": 0, "--wait-for-video": 1, "--hls-use-mpegts": 0,
    "--playlist-items": 1, "-I": 1, "--playlist-reverse": 0, "--playlist-random": 0,
    "-i": 0, "--ignore-errors": 0, "--abort-on-error": 0,
}


def extra_args(text):
    """The user's extra yt-dlp arguments, if every one is allowed."""
    import shlex
    try:
        words = shlex.split(text)
    except ValueError as e:
        raise RuntimeError(f"Extra yt-dlp arguments can't be read: {e}") from None
    out, i = [], 0
    while i < len(words):
        word = words[i]
        # --option=value counts as the option and its value.
        name, eq, value = word.partition("=") if word.startswith("--") else (word, "", "")
        takes = EXTRA_ALLOWED.get(name)
        if takes is None or (eq and takes != 1):
            raise RuntimeError(f"Extra yt-dlp argument {name!r} isn't allowed. "
                               "VidGrab's settings allow download, format, subtitle and file-name options only.")
        if eq:
            out += [name, value]
        elif takes:
            if i + 1 >= len(words):
                raise RuntimeError(f"Extra yt-dlp argument {name} needs a value.")
            out += [name, words[i + 1]]
            i += 1
        else:
            out.append(name)
        i += 1
    return out


def safe_template(template):
    """An output template that stays inside the download folder."""
    parts = re.split(r"[/\\]", template)
    if template.startswith(("/", "~")) or ".." in parts:
        raise RuntimeError("The file name template must stay inside the download folder "
                           "(no leading / or ~, no ..).")
    return template


def build_download_cmd(ytdlp, msg):
    opts = msg.get("options", {})
    out_dir = os.path.expanduser(opts.get("outputDir") or default_download_dir())
    os.makedirs(out_dir, exist_ok=True)
    template = safe_template(opts.get("filenameTemplate") or DEFAULT_TEMPLATE)
    if template == OLD_DEFAULT_TEMPLATE:
        template = DEFAULT_TEMPLATE
    if msg.get("pageTitle"):
        template = page_title_template(msg["pageTitle"], out_dir)

    cmd = ytdlp + [
        "--newline",
        "--progress",
        "--no-simulate",
        "--no-colors",
        "--progress-template",
        "download:[vg-progress] %(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s|%(info.format_id)s",
        "--print", "video:[vg-title] %(title)s",
        "--print", "after_move:[vg-file] %(filepath)s",
        "-P", out_dir,
        "-o", template,
    ]
    preset = msg.get("preset", "best")
    # Without ffmpeg, yt-dlp can't merge separate video and audio, convert
    # or embed anything: take the best single file as it comes instead.
    ffmpeg = shutil.which("ffmpeg", path=search_path())
    if not ffmpeg and preset in ("audio_mp3", "audio_m4a"):
        raise RuntimeError("Converting to MP3 or M4A needs ffmpeg (sudo apt install ffmpeg). "
                           "\"Audio only (original)\" works without it.")
    if ffmpeg:
        cmd += PRESETS.get(preset, PRESETS["best"])
    elif preset == "audio_best":
        cmd += ["-f", "ba/b"]
    else:
        height = preset if preset.isdigit() else None
        cmd += ["-f", f"b[height<={height}][ext=mp4]/b[height<={height}]/b" if height else "b[ext=mp4]/b"]
    if not opts.get("allowPlaylist"):
        cmd.append("--no-playlist")
    if opts.get("embedMetadata") and ffmpeg:
        cmd += ["--embed-metadata", "--embed-thumbnail"]
    if opts.get("subtitles"):
        # A stream's subtitle tracks are often named ("English") rather
        # than coded ("en"), so take all of them there.
        langs = "all" if msg.get("pageTitle") else (opts.get("subLangs") or "en.*")
        cmd += ["--write-subs", "--sub-langs", langs] + (["--embed-subs"] if ffmpeg else [])
    if opts.get("sponsorblock"):
        cmd += ["--sponsorblock-remove", "sponsor,selfpromo,interaction"]
    if opts.get("useCookies") == "always":
        cmd += cookie_args(opts)
    js = find_js_runtime()
    if js:
        cmd += ["--js-runtimes", f"{js[0]}:{js[1]}"]
    if msg.get("referer"):
        cmd += ["--referer", msg["referer"]]
    if msg.get("userAgent"):
        # Matching the browser keeps sites from serving a different page.
        cmd += ["--user-agent", msg["userAgent"]]
    cmd += extra_args(opts.get("extraArgs") or "")
    cmd += ["--", msg["url"]]
    return cmd, out_dir


# yt-dlp errors that mean the site-specific extractor is broken or out of
# date; the generic extractor can often still find the <video> on the page.
# Errors a logged-in browser session usually fixes (bot checks, age gates,
# members-only and private videos): only yt-dlp's own words for those, so a
# page that merely says "log in", or a plain 403, doesn't hand the site your
# browser's cookies.
NEEDS_LOGIN = re.compile(r"Use --cookies|Sign in to confirm|confirm your age|age-restricted|members-only|"
                         r"Private video\. Sign in|only available for registered users|requires authentication", re.I)
EXTRACTOR_BROKEN = re.compile(r"Unable to extract|please report this issue|No video formats found|Unsupported URL")


def save_cookies(text):
    """Write cookies.txt text to a private temporary file; returns its path."""
    folder = os.environ.get("XDG_RUNTIME_DIR") or None
    fd, path = tempfile.mkstemp(prefix="vidgrab-cookies-", suffix=".txt", dir=folder)
    with os.fdopen(fd, "w") as f:
        f.write(text)
    return path


def handle_download(msg):
    job_id = msg["id"]
    # Only the host names a cookies file (and deletes it afterwards).
    msg.setdefault("options", {}).pop("cookiesFile", None)
    if msg.get("cookies"):
        msg.setdefault("options", {})["cookiesFile"] = save_cookies(msg["cookies"])
    ytdlp = find_ytdlp(msg.get("options", {}).get("ytdlpPath"))
    if not ytdlp:
        send({"type": "job", "id": job_id, "state": "error", "error": "yt-dlp not found. Run install.sh --fetch-ytdlp."})
        return
    try:
        cmd, out_dir = build_download_cmd(ytdlp, msg)
    except Exception as e:  # noqa: BLE001
        send({"type": "job", "id": job_id, "state": "error", "error": str(e)})
        return
    send({"type": "job", "id": job_id, "state": "running", "dir": out_dir})
    subs = [s for s in msg.get("subtitles") or [] if str(s.get("url", "")).startswith(("https://", "http://"))][:20]
    threading.Thread(target=run_job_then_forget_cookies, args=(job_id, cmd, msg.get("options", {}), subs, msg), daemon=True).start()


def run_job_then_forget_cookies(job_id, cmd, opts, subs=(), msg=None):
    try:
        run_job(job_id, cmd, opts, subs, msg or {})
    finally:
        if opts.get("cookiesFile"):
            try:
                os.remove(opts["cookiesFile"])
            except OSError:
                pass


def is_failure(code):
    return code > 0 and code != 130


def with_template(cmd, template):
    i = cmd.index("-o")
    return cmd[:i + 1] + [template] + cmd[i + 2:]


SUB_LIMIT = 4 * 1024 * 1024


def save_subtitles(video, subs, msg):
    """Save a stream's subtitle files next to the video, as "<video>.<lang>.vtt",
    which VLC, mpv and most players load with it. Returns the files saved."""
    base = os.path.splitext(video)[0]
    headers = {"User-Agent": msg.get("userAgent") or "Mozilla/5.0"}
    if msg.get("referer"):
        headers["Referer"] = msg["referer"]
    saved, used = [], set()
    for n, s in enumerate(subs, 1):
        try:
            req = urllib.request.Request(s["url"], headers=headers)
            with urllib.request.urlopen(req, timeout=30) as r:
                data = r.read(SUB_LIMIT + 1)
        except Exception:  # noqa: BLE001
            continue
        if len(data) > SUB_LIMIT or not data.strip():
            continue
        text = data.lstrip(b"\xef\xbb\xbf").lstrip()
        ext = "vtt" if text.startswith(b"WEBVTT") else "srt" if re.match(rb"\d+\s*\r?\n\d\d:", text) else None
        # One piece of an HLS subtitle playlist, not a whole file.
        if not ext or b"X-TIMESTAMP-MAP" in text[:300]:
            continue
        lang = re.sub(r"[^a-zA-Z0-9-]", "", str(s.get("lang") or ""))[:12] or f"sub{n}"
        name = lang
        i = 2
        while name in used:
            name, i = f"{lang}{i}", i + 1
        used.add(name)
        path = f"{base}.{name}.{ext}"
        try:
            with open(path, "wb") as f:
                f.write(data)
        except OSError:
            continue
        saved.append(vtt_to_srt(path) if ext == "vtt" else path)
    return saved


def vtt_to_srt(path):
    """Most desktop players read SRT; WebVTT much less often. Returns the
    SRT file, or the VTT file when ffmpeg isn't there or can't convert it."""
    ffmpeg = shutil.which("ffmpeg", path=search_path())
    if not ffmpeg:
        return path
    srt = path[:-4] + ".srt"
    try:
        ok = subprocess.run([ffmpeg, "-nostdin", "-loglevel", "error", "-y", "-i", path, srt],
                            capture_output=True, timeout=60).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        ok = False
    if ok and os.path.getsize(srt) > 0:
        os.remove(path)
        return srt
    try:
        os.remove(srt)
    except OSError:
        pass
    return path


# ISO 639-2 codes players show as the track's language.
LANG3 = {"en": "eng", "english": "eng", "es": "spa", "spanish": "spa", "fr": "fra", "fre": "fra", "french": "fra",
         "de": "deu", "ger": "deu", "german": "deu", "it": "ita", "italian": "ita", "pt": "por", "portuguese": "por",
         "ja": "jpn", "japanese": "jpn", "ko": "kor", "korean": "kor", "zh": "zho", "chi": "zho", "chinese": "zho",
         "ru": "rus", "russian": "rus", "ar": "ara", "arabic": "ara", "hi": "hin", "hindi": "hin", "nl": "nld",
         "dut": "nld", "tr": "tur", "pl": "pol", "sv": "swe", "id": "ind", "vi": "vie", "th": "tha", "uk": "ukr",
         "cs": "ces", "cze": "ces", "el": "ell", "gre": "ell", "he": "heb", "ro": "ron", "rum": "ron", "hu": "hun",
         "da": "dan", "fi": "fin", "no": "nor", "nb": "nob"}


def embed_subtitles(video, subs):
    """Put subtitle files into the video as selectable tracks. Returns True
    when it worked (the separate files are then removed)."""
    ffmpeg = shutil.which("ffmpeg", path=search_path())
    if not ffmpeg or not subs:
        return False
    root, ext = os.path.splitext(video)
    tmp = f"{root}.subs-tmp{ext}"
    cmd = [ffmpeg, "-nostdin", "-loglevel", "error", "-y", "-i", video]
    for f in subs:
        cmd += ["-i", f]
    cmd += ["-map", "0"] + [x for i in range(len(subs)) for x in ("-map", str(i + 1))]
    # MP4 only takes subtitles as mov_text; MKV and WebM keep their own kind.
    codec = "mov_text" if ext.lower() in (".mp4", ".m4v", ".mov") else "srt" if ext.lower() == ".mkv" else "webvtt"
    cmd += ["-c", "copy", "-c:s", codec]
    for i, f in enumerate(subs):
        lang = f.rsplit(".", 2)[-2].lower() if f.count(".") >= 2 else ""
        lang = LANG3.get(re.sub(r"-.*", "", lang), lang if len(lang) == 3 else "")
        if lang:
            cmd += [f"-metadata:s:s:{i}", f"language={lang}"]
    cmd.append(tmp)
    try:
        ok = subprocess.run(cmd, capture_output=True, timeout=600).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        ok = False
    if not ok or not os.path.getsize(tmp):
        try:
            os.remove(tmp)
        except OSError:
            pass
        return False
    os.replace(tmp, video)
    for f in subs:
        try:
            os.remove(f)
        except OSError:
            pass
    return True


VIDEO_FILE = re.compile(r"\.(mp4|mkv|webm|mov|m4v)$", re.I)


def downloaded_video(files, cmd, since):
    """The video file a job made: the one yt-dlp reported if it's there,
    else the newest video in the download folder written since the start."""
    for f in reversed(files):
        if VIDEO_FILE.search(f) and os.path.isfile(f):
            return f
    folder = cmd[cmd.index("-P") + 1] if "-P" in cmd else None
    if not folder or not os.path.isdir(folder):
        return None
    found = [e for e in os.scandir(folder) if e.is_file() and VIDEO_FILE.search(e.name) and e.stat().st_mtime >= since]
    return max(found, key=lambda e: e.stat().st_mtime).path if found else None


def run_job(job_id, cmd, opts, subs=(), msg=None):
    started = time.time() - 2
    code, last_err, files = run_ytdlp(job_id, cmd)
    if is_failure(code) and NEEDS_LOGIN.search(last_err) and opts.get("useCookies", "auto") == "auto" \
            and "--cookies-from-browser" not in cmd and "--cookies" not in cmd:
        args = cookie_args(opts)
        if args:
            send({"type": "job", "id": job_id, "state": "running", "percent": 0,
                  "stage": "Site wants a login, retrying with your browser cookies…"})
            sep = cmd.index("--")
            cmd = cmd[:sep] + args + cmd[sep:]
            code, last_err, files = run_ytdlp(job_id, cmd)
    if is_failure(code) and EXTRACTOR_BROKEN.search(last_err) \
            and "--force-generic-extractor" not in cmd:
        send({"type": "job", "id": job_id, "state": "running", "percent": 0,
              "stage": "Site support failed, trying generic page scan…"})
        sep = cmd.index("--")
        cmd = cmd[:sep] + ["--force-generic-extractor"] + cmd[sep:]
        code, generic_err, files = run_ytdlp(job_id, cmd)
        if is_failure(code):
            last_err = f"{last_err}\nGeneric fallback: {generic_err or f'exit code {code}'}"
    if is_failure(code) and "File name too long" in last_err:
        send({"type": "job", "id": job_id, "state": "running", "percent": 0,
              "stage": "File name too long, retrying with a shorter name…"})
        code, last_err, files = run_ytdlp(job_id, with_template(cmd, SHORT_TEMPLATE))
    if code == 0:
        video = downloaded_video(files, cmd, started) if subs else None
        if subs and video:
            send({"type": "job", "id": job_id, "state": "running", "stage": "Saving subtitles…"})
            saved = save_subtitles(video, subs, msg or {})
            if not embed_subtitles(video, saved):
                files = files + saved
        send({"type": "job", "id": job_id, "state": "done", "percent": 100, "files": files})
    elif code < 0 or code == 130:
        send({"type": "job", "id": job_id, "state": "cancelled"})
    else:
        send({"type": "job", "id": job_id, "state": "error", "error": last_err or f"yt-dlp exited with code {code}"})


def run_ytdlp(job_id, cmd):
    """Runs yt-dlp, streaming progress to the browser. Returns (code, last error, files)."""
    try:
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=env_with_paths(),
            start_new_session=True,
        )
    except Exception as e:  # noqa: BLE001
        return 1, str(e), []
    with jobs_lock:
        jobs[job_id] = proc
    last_err = ""
    files = []
    for line in proc.stdout:
        line = line.rstrip("\n")
        if line.startswith("[vg-progress] "):
            parts = line[len("[vg-progress] "):].split("|")
            pct = parts[0].strip().rstrip("%")
            try:
                pct = float(pct)
            except ValueError:
                pct = None
            send({
                "type": "job", "id": job_id, "state": "running",
                "percent": pct,
                "speed": parts[1].strip() if len(parts) > 1 else "",
                "eta": parts[2].strip() if len(parts) > 2 else "",
            })
        elif line.startswith("[vg-title] "):
            send({"type": "job", "id": job_id, "state": "running", "title": line[len("[vg-title] "):]})
        elif line.startswith("[vg-file] "):
            files.append(line[len("[vg-file] "):])
        elif line.startswith("[Merger]") or line.startswith("[ExtractAudio]") or line.startswith("[FixupM3u8]"):
            send({"type": "job", "id": job_id, "state": "running", "stage": "Processing…"})
        elif line.startswith("ERROR:"):
            last_err = line
        elif line.startswith("WARNING:"):
            send({"type": "job", "id": job_id, "state": "running", "warning": line})
    code = proc.wait()
    with jobs_lock:
        jobs.pop(job_id, None)
    # A cancel during the first attempt must not trigger the fallback.
    if code == 1 and "Interrupted by user" in last_err:
        code = 130
    return code, last_err, files


def handle_cancel(msg):
    with jobs_lock:
        proc = jobs.get(msg["id"])
    if proc and proc.poll() is None:
        try:
            os.killpg(proc.pid, signal.SIGINT)
        except ProcessLookupError:
            pass


def install_ytdlp(update=False):
    """Fetch the official yt-dlp release into DATA_DIR (as install.sh --fetch-ytdlp does),
    or with `update` replace the one there if a newer release is out. The file
    is kept only if its hash is in the release's signed list (see
    YTDLP_RELEASE); nothing fetched is run before that. Returns (ok, message)."""
    with install_lock:
        if not update and os.access(OWN_YTDLP, os.X_OK):
            return True, f"yt-dlp is in {OWN_YTDLP}"
        tmp = OWN_YTDLP + ".tmp"
        try:
            os.makedirs(DATA_DIR, exist_ok=True)
            sums = fetch(YTDLP_RELEASE + "SHA2-256SUMS", 1 << 16)
            signed_at = verify_signature(sums, fetch(YTDLP_RELEASE + "SHA2-256SUMS.sig", 1 << 14))
            if signed_at < last_signed():
                raise ValueError("the release offered is older than the one already seen")
            want = listed_sha256(sums, "yt-dlp")
            if os.access(OWN_YTDLP, os.X_OK) and file_sha256(OWN_YTDLP) == want:
                touch(UPDATE_STAMP)
                return True, "yt-dlp is up to date"
            data = fetch(YTDLP_RELEASE + "yt-dlp", 64 << 20)
            if hashlib.sha256(data).hexdigest() != want:
                raise ValueError("the download doesn't match yt-dlp's signed list of hashes; try again later")
            with open(tmp, "wb") as f:
                f.write(data)
            os.chmod(tmp, 0o755)
            os.replace(tmp, OWN_YTDLP)
            with open(SIGNED_STAMP, "w") as f:
                f.write(str(signed_at))
            touch(UPDATE_STAMP)
            code, out, err = run_capture([OWN_YTDLP, "--version"], timeout=60)
            return code == 0, f"Installed yt-dlp {out} to {OWN_YTDLP}" if code == 0 else err
        except Exception as e:  # noqa: BLE001
            try:
                os.remove(tmp)
            except OSError:
                pass
            return False, f"Couldn't install yt-dlp: {e}"


def install_then_status(msg):
    ok, output = install_ytdlp()
    if not ok:
        send({"type": "status", "hostVersion": HOST_VERSION, "error": output})
        return
    handle_status(dict(msg, autoInstall=False))


def handle_install(msg):
    ok, output = install_ytdlp()
    send({"type": "update", "ok": ok, "output": output})


def touch(path):
    with open(path, "a"):
        os.utime(path, None)


def update_if_due():
    """Keep VidGrab's own yt-dlp current: sites change, yt-dlp follows within days."""
    try:
        if time.time() - os.path.getmtime(UPDATE_STAMP) < UPDATE_EVERY:
            return
    except OSError:
        pass
    if install_lock.locked():
        return
    try:
        touch(UPDATE_STAMP)
        # Not yt-dlp's own updater: that one doesn't check the signature.
        install_ytdlp(update=True)
    except Exception:  # noqa: BLE001
        pass


def folder_to_show(path):
    """The folder Open shows for `path`: itself if it's a folder, otherwise the
    one it's in. Never a file: opening one would run whatever opens it."""
    path = os.path.realpath(os.path.expanduser(path))
    if not os.path.isdir(path):
        path = os.path.dirname(path)
    return path if os.path.isdir(path) else None


def handle_open(msg):
    folder = folder_to_show(msg.get("path") or default_download_dir())
    if folder:
        subprocess.Popen(["xdg-open", folder], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)


HANDLERS = {
    "status": handle_status,
    "update": lambda m: threading.Thread(target=handle_update, args=(m,), daemon=True).start(),
    "install": lambda m: threading.Thread(target=handle_install, args=(m,), daemon=True).start(),
    "download": handle_download,
    "cancel": handle_cancel,
    "open": handle_open,
}


def main():
    while True:
        msg = read_message()
        if msg is None:
            break
        handler = HANDLERS.get(msg.get("action"))
        if handler is None:
            send({"type": "error", "error": f"unknown action {msg.get('action')!r}"})
            continue
        try:
            handler(msg)
        except Exception as e:  # noqa: BLE001
            send({"type": "error", "error": str(e), "id": msg.get("id")})
    # Browser closed the port: stop any running downloads.
    with jobs_lock:
        for proc in jobs.values():
            try:
                os.killpg(proc.pid, signal.SIGINT)
            except ProcessLookupError:
                pass


if __name__ == "__main__":
    # install.sh --fetch-ytdlp: the same checked fetch, from a terminal. (The
    # browser starts this with a manifest's path first, never this.)
    if sys.argv[1:2] == ["--fetch-ytdlp"]:
        ok, said = install_ytdlp(update=True)
        print(said)
        sys.exit(0 if ok else 1)
    main()
