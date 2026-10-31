"""Checks of the VidGrab helper's settings rules (run by .github/build-linux.sh
with WISP_CHECK=1): settings must never run other programs or touch files
outside the download folder."""

import os
import sys
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


if __name__ == "__main__":
    unittest.main()
