"""Checks of the VidGrab helper's settings rules (run by .github/build-linux.sh
with WISP_CHECK=1): settings must never run other programs or touch files
outside the download folder."""

import base64
import hashlib
import os
import shutil
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "plugins", "vidgrab", "host"))
import vidgrab_host as v  # noqa: E402


class ExtraArgs(unittest.TestCase):
    def test_allowed(self):
        self.assertEqual(v.extra_args(""), [])
        self.assertEqual(v.extra_args("--limit-rate 5M -N 4"), ["--limit-rate", "5M", "-N", "4"])
        self.assertEqual(v.extra_args("--format=bv*+ba --no-mtime"), ["--format", "bv*+ba", "--no-mtime"])
        self.assertEqual(v.extra_args("-f 'bv*[height<=720]+ba'"), ["-f", "bv*[height<=720]+ba"])

    def test_refused(self):
        for text in [
            "--exec 'rm -rf ~'", "--exec=touch /tmp/x", "--exe x", "--exec-before-download x",
            "-o /etc/x", "-P /", "--paths /", "--config-locations /tmp/c", "--downloader aria2c",
            "--downloader-args x", "--postprocessor-args x", "--ppa x", "--use-postprocessor x",
            "--plugin-dirs /tmp", "--batch-file f", "-a f", "--cookies /tmp/c", "--load-info-json f",
            "--ffmpeg-location /tmp/x", "https://example.com", "-N", "--no-mtime=1", "'unclosed",
        ]:
            with self.subTest(text=text), self.assertRaises(RuntimeError):
                v.extra_args(text)

    def test_values_are_not_options(self):
        # A value that looks like an option stays the value of the one before.
        self.assertEqual(v.extra_args("-f --exec"), ["-f", "--exec"])


class Template(unittest.TestCase):
    def test_inside_folder(self):
        for t in ["%(title)s.%(ext)s", "%(uploader)s/%(title)s.%(ext)s"]:
            self.assertEqual(v.safe_template(t), t)

    def test_escapes(self):
        for t in ["/etc/%(title)s", "~/x", "../x", "a/../b", "a\\..\\b"]:
            with self.subTest(t=t), self.assertRaises(RuntimeError):
                v.safe_template(t)


class YtdlpPath(unittest.TestCase):
    def test_only_ytdlp(self):
        self.assertTrue(v.is_ytdlp_name("/home/me/bin/yt-dlp"))
        self.assertTrue(v.is_ytdlp_name("/opt/yt-dlp_linux"))
        for p in ["/bin/sh", "/usr/bin/python3", "/tmp/yt-dlp.sh", "/tmp/not-yt-dlp"]:
            self.assertFalse(v.is_ytdlp_name(p), p)
        self.assertNotEqual(v.find_ytdlp("/bin/sh"), ["/bin/sh"])


class CookiesFile(unittest.TestCase):
    def test_settings_cannot_name_one(self):
        sent = []
        v.send, v.find_ytdlp = sent.append, lambda *_: None
        msg = {"id": "j", "url": "https://example.com", "options": {"cookiesFile": "/home/me/precious"}}
        v.handle_download(msg)
        self.assertNotIn("cookiesFile", msg["options"])



# yt-dlp's list of hashes for its release 2026.10.26 and the signature
# published with it (SHA2-256SUMS and SHA2-256SUMS.sig), in base64.
SUMS = base64.b64decode(
    "ZTUxZTY5ZTg5NDg5ZjE5OTUzMGYxM2QzMmE0MzQyZGEzZGRiMmNmMzQ4NDJmMzY0NjAzYTA2NmRiMDVhZmJmMyAgeXQtZGxwCmY1"
    "Mzg5ZDM2OThlY2RlNGVmMDZmNGU2YjFjZTAyOTRmZTlhYzEzOTYyMjhjODk5MGI4ZDM0MTZkNmQxNzdhN2QgIHl0LWRscC5leGUK"
    "MjFmOTQyZDI1OGQyNWNjNGU2MGI0OTgyYTI1YzQyMzQ2MGYwYmViYjgzNWYzOGU2ZGZkMWZjYjE0MDA4N2Y5NSAgeXQtZGxwLnRh"
    "ci5negowMDY2ZGJhYzNjMTI0MDI4ZTJkY2FlMmExOTdmY2NlZTMzY2Q5MWY0NTYzMTNkMjRmN2FmODlmZDU0NTkwZTk4ICB5dC1k"
    "bHBfYXJtNjQuZXhlCjc2N2JhOWRjZTcwNTJhYTE5NmY4YjEyNjUyZjQzM2Q3NmRlMTI4YmExNjg5NjZmYTg0YjYzOTRkNWU2YzY3"
    "OTUgIHl0LWRscF9saW51eApmMzQwNTM0MTUxMzZlNTUwNzNkZDY0YmE1MzI2ZDE2MWVjYzIzY2UwZTk4YzA4MWQ4NzUyODY3YjM3"
    "ODA5NDUwICB5dC1kbHBfbGludXguemlwCmU4N2U5NTMzZTg5ZmU1ODg5YWJhYTIxZjUxMTRkZmU3NzU2ZDZhYTYyMTliODM1NTFh"
    "ZGQwNGQ0OGIxOGU0ODMgIHl0LWRscF9saW51eF9hYXJjaDY0Cjc0NzA4ZTcwN2U4ZmE4MTc3YzgzNDdlNTgxNTIzMzdlZGYzMjAx"
    "NDljZDdiMjI1ZGVmZDQ1M2Y3Y2RlNDc4MjIgIHl0LWRscF9saW51eF9hYXJjaDY0LnppcAplYzgyOGMyNmM2ZGE3ODk4YjI3OWRh"
    "MmNhZDNkY2E0MzRlYmVjN2Y4ZjE4YWU3YjY0MjAwMjdmM2FhMDM3YmYyICB5dC1kbHBfbGludXhfYXJtdjdsLnppcAowZjkyMmMx"
    "NWNjYzBiODNlZWZhMWM4YmM5MTA0ZTQzNDMxOGM0YmYxNmU0MDhlMWVlOTNiNzc0NWE2ZWZlMGFhICB5dC1kbHBfbWFjb3MKMjBk"
    "OThmYTYwODk5OGVmZjg3ZDFkMzVhNmU5ZmI2NWU0Yjg3NDYzOWE2Y2M4OWFkZGMxOWRhM2JlY2RlM2VmMSAgeXQtZGxwX21hY29z"
    "LnppcAplOTdhNGRkOTE5MjIxZDIzM2U3NzIwZTE4ZmEzZjY2ZGI2MGZiZDFkOGMwODQ2YmY2ZGViOGU4OGUwNDQ3NWE4ICB5dC1k"
    "bHBfbXVzbGxpbnV4CmMzYTY3ZjdjMTRkNGNiZjg3NmU3ODAxMDM5MjM3ZmMxNmI5MjlhNjI5YzcyZjljNzg1ZWMzZmRjOGFhNDA3"
    "NDggIHl0LWRscF9tdXNsbGludXguemlwCmVhOTdiODU2NDY2N2YzY2FiYzMzNGMzMjE2NTI2ZGUzOWJjNjg0ZTk5ODE1ZWNmMDdi"
    "NjY0MmY0Y2JhOGFmNGQgIHl0LWRscF9tdXNsbGludXhfYWFyY2g2NAoxNjQ4YmVlYjdiMGM3OGI0NzE2NTQyNjI2OTcwNjZjMDE2"
    "NzgzZDMzODEzNjU5MzY3YTVjMjc0ODI4YmEzMjQwICB5dC1kbHBfbXVzbGxpbnV4X2FhcmNoNjQuemlwCmZkNTFiMjEyY2Y4Mzgw"
    "YzdhNTJlZTA4YzBkNmJlN2YzYzU5YjJmMmM4MzJlYWY3YjQ1YWNmYWU2ZWFiNTc0MTkgIHl0LWRscF93aW4uemlwCjVlYWE0YzYx"
    "ZjNjMDkxODlmM2E1NTFiZjE4NTVjMTNhYWQ2MWZiODY1Y2MzM2YwNjAxMmE2YzJkYmUyODdhNDUgIHl0LWRscF93aW5fYXJtNjQu"
    "emlwCmUyODk4YWZkNTFjMDI5ZDIzYzU4ZGU2MGNkODQ0NmIxMTBkZDNhYTEyMWIzZTQxNDliYWZjZWUzMzU2YzU5YzAgIHl0LWRs"
    "cF93aW5feDg2LnppcApjMDQwN2M2NjM2MWY1MGE5OWUxMDk0OWFlODgyZWEyYWQ3ZGU5MDg2Yjc1ZDIyNmQ4ZmQ4OGRhNGY3NDgy"
    "OGZhICB5dC1kbHBfeDg2LmV4ZQo="
)
SIG = base64.b64decode(
    "iQIzBAABCgAdFiEErAy75oSNaoc0ZK9OV89lkztadYEFAmrf0ykACgkQV89lkztadYFrgRAAi88f07ETAUGIzH/ifgZLMdYcQavM"
    "pBw5GsbCi4Os7K2+MJNZgJ1SpTYa/qQZ0GPS+RCsebOPcbByTAt9OSjnN7ZzJVPbl8B/FioDRWBond7Z3JvZojU28Xv6dd0YGv7s"
    "w0pNit1Rro+9qtxQ1ibiNlqZHyj8BHyANOvSNNRLJuVmDkXWirmQwAjHnTmnK3Qcg2dOmuQr5DX/XbeNqzxxcqphUO7upijylfOt"
    "VQyKoY91ze41k0XE4H1e+jdn8OuZEZhw2h/UEk9EX5ENn6jAqOPN3c0t33vUgZqYUliDnYogSC2ITb8tbsxdMRnQG5HouYLHVK56"
    "aPClkx5sm/891XpwYw2xF8RIqqDEt9yf+V0xbd2rJBOvjQmPMJ6iLUbaBI440VRaJDd5ZEkMAGd+O8xxMQ+xf/SvPpFM8ytAgxk+"
    "B76xTYv/2Z9FiMC08Au0iXkEaJ72ws/i0Ly55BvZ8bbON3yMrk64WG3JHoQo0xF+1K1YuWjpXxkesseLlj6fPcNlcAKjK2mzKV5E"
    "Qe+ZPdCzWy4+BM3tQybyrUkHYQkJnCUad1a2/ufpnZFy1sFm/SM+o71Sf3qLYFz/BP6RVSnChN5DvuxN+hPZfL4dqubxUIXZAAYx"
    "t1dss2NhqBc7BdhbetJwmt/foI8NLAiKYp6D5Rhv1zhdajQta+BlF78="
)


class Signature(unittest.TestCase):
    def test_yt_dlps_own_signature_is_good(self):
        # Signed on 26 October 2026.
        self.assertEqual(v.verify_signature(SUMS, SIG), 1793053481)
        self.assertEqual(v.listed_sha256(SUMS, "yt-dlp"), "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3")

    def test_a_changed_list_is_refused(self):
        changed = SUMS.replace(b"e51e69e8", b"e51e69e9")
        self.assertNotEqual(changed, SUMS)
        for data in [changed, SUMS + b"\n", SUMS[:-1], b""]:
            with self.subTest(data=data[:20]), self.assertRaises(ValueError):
                v.verify_signature(data, SIG)

    def test_a_changed_signature_is_refused(self):
        # The signer's key ID sits in the part of an OpenPGP signature that
        # isn't signed (it's only a hint for finding the key): skipped here.
        hashed_end = 3 + 6 + int.from_bytes(SIG[7:9], "big")
        unsigned = range(hashed_end + 2, hashed_end + 2 + int.from_bytes(SIG[hashed_end:hashed_end + 2], "big"))
        for at in range(len(SIG)):
            if at in unsigned:
                continue
            changed = SIG[:at] + bytes([SIG[at] ^ 1]) + SIG[at + 1:]
            with self.subTest(at=at), self.assertRaises(ValueError):
                v.verify_signature(SUMS, changed)
        for sig in [b"", b"\x89", SIG[:40], SIG + b"\x00", SIG + SIG, b"\x00" * len(SIG)]:
            with self.subTest(sig=sig[:8]), self.assertRaises(ValueError):
                v.verify_signature(SUMS, sig)

    def test_a_signature_by_another_key_is_refused(self):
        real = v.YTDLP_KEY_N
        try:
            v.YTDLP_KEY_N = real + 2
            with self.assertRaises(ValueError):
                v.verify_signature(SUMS, SIG)
        finally:
            v.YTDLP_KEY_N = real

    def test_only_the_named_file_is_looked_up(self):
        sums = b"aa  yt-dlp\n" + b"1" * 64 + b"  yt-dlp.exe\n" + b"2" * 64 + b" *yt-dlp\n"
        self.assertEqual(v.listed_sha256(sums, "yt-dlp"), "2" * 64)
        with self.assertRaises(ValueError):
            v.listed_sha256(sums, "yt-dlp_linux")


class Install(unittest.TestCase):
    """Fetching yt-dlp, with the network stood in for."""

    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.saved = (v.DATA_DIR, v.OWN_YTDLP, v.UPDATE_STAMP, v.SIGNED_STAMP, v.fetch, v.run_capture)
        v.DATA_DIR = self.dir
        v.OWN_YTDLP = os.path.join(self.dir, "yt-dlp")
        v.UPDATE_STAMP = os.path.join(self.dir, "last-update-check")
        v.SIGNED_STAMP = os.path.join(self.dir, "yt-dlp-signed")
        self.ran = []
        v.run_capture = lambda cmd, timeout=60: (self.ran.append(cmd), (0, "test", ""))[1]

    def tearDown(self):
        v.DATA_DIR, v.OWN_YTDLP, v.UPDATE_STAMP, v.SIGNED_STAMP, v.fetch, v.run_capture = self.saved
        shutil.rmtree(self.dir)

    def serve(self, binary, sums=SUMS, sig=SIG):
        files = {"SHA2-256SUMS": sums, "SHA2-256SUMS.sig": sig, "yt-dlp": binary}
        v.fetch = lambda url, limit: files[url.rsplit("/", 1)[1]]

    def test_a_download_not_in_the_signed_list_is_never_kept_or_run(self):
        self.serve(b"#!/bin/sh\necho not yt-dlp\n")
        ok, message = v.install_ytdlp()
        self.assertFalse(ok)
        self.assertIn("doesn't match", message)
        self.assertEqual(os.listdir(self.dir), [])
        self.assertEqual(self.ran, [])

    def test_an_unsigned_list_is_refused_before_anything_else_is_fetched(self):
        binary = b"#!/bin/sh\n"
        sums = hashlib.sha256(binary).hexdigest().encode() + b"  yt-dlp\n"
        self.serve(binary, sums=sums)
        asked = []
        files = v.fetch
        v.fetch = lambda url, limit: (asked.append(url.rsplit("/", 1)[1]), files(url, limit))[1]
        ok, _ = v.install_ytdlp()
        self.assertFalse(ok)
        self.assertEqual(asked, ["SHA2-256SUMS", "SHA2-256SUMS.sig"])
        self.assertEqual(os.listdir(self.dir), [])
        self.assertEqual(self.ran, [])

    def test_an_older_release_than_the_one_seen_is_refused(self):
        with open(v.SIGNED_STAMP, "w") as f:
            f.write(str(1793053481 + 1))
        self.serve(b"x")
        ok, message = v.install_ytdlp()
        self.assertFalse(ok)
        self.assertIn("older", message)



class NeedsLogin(unittest.TestCase):
    def test_yt_dlps_words(self):
        for err in ["ERROR: [youtube] abc: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies",
                    "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate",
                    "ERROR: [youtube] abc: Join this channel to get access to members-only content",
                    "ERROR: [youtube] abc: Private video. Sign in if you've been granted access to this video"]:
            self.assertTrue(v.NEEDS_LOGIN.search(err), err)

    def test_not_for_any_page(self):
        for err in ["ERROR: unable to download video data: HTTP Error 403: Forbidden",
                    "ERROR: [generic] Please log in to continue",
                    "ERROR: Unsupported URL: https://example.com/login"]:
            self.assertFalse(v.NEEDS_LOGIN.search(err), err)


class OpenShowsFolders(unittest.TestCase):
    def test_only_folders(self):
        d = tempfile.mkdtemp()
        try:
            f = os.path.join(d, "clip.mp4")
            open(f, "w").close()
            self.assertEqual(v.folder_to_show(f), os.path.realpath(d))
            self.assertEqual(v.folder_to_show(d), os.path.realpath(d))
            evil = os.path.join(d, "run.desktop")
            open(evil, "w").close()
            self.assertEqual(v.folder_to_show(evil), os.path.realpath(d))
            self.assertIsNone(v.folder_to_show("/no/such/place/x"))
        finally:
            shutil.rmtree(d)

if __name__ == "__main__":
    unittest.main()
