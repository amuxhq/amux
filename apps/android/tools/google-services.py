#!/usr/bin/env python3
"""Write the Android resources Firebase reads from google-services.json.

Usage: google-services.py PACKAGE JSON OUT_DIR

What Google's google-services Gradle plugin did for the app: Firebase starts
from string resources (`google_app_id`, `gcm_defaultSenderId`, ...) that the
plugin wrote from the Firebase console's google-services.json for the app's
package. This writes the same resources under OUT_DIR/values/, for the build
to carry when the file is present (apps/android/app/BUCK).
"""

from __future__ import annotations

import json
from pathlib import Path
import sys
from xml.sax.saxutils import escape


def resources(config: dict, package: str) -> dict[str, str]:
    """The resources for `package`'s client in `config`."""
    project = config.get("project_info", {})
    client = next(
        (
            c
            for c in config.get("client", [])
            if c.get("client_info", {}).get("android_client_info", {}).get("package_name") == package
        ),
        None,
    )
    if client is None:
        raise SystemExit(f"google-services: google-services.json has no client for {package}")
    values = {
        "google_app_id": client["client_info"]["mobilesdk_app_id"],
        "gcm_defaultSenderId": project.get("project_number"),
        "project_id": project.get("project_id"),
        "google_storage_bucket": project.get("storage_bucket"),
        "firebase_database_url": project.get("firebase_url"),
    }
    keys = client.get("api_key", [])
    if keys:
        values["google_api_key"] = keys[0].get("current_key")
        values["google_crash_reporting_api_key"] = keys[0].get("current_key")
    web = [o for o in client.get("oauth_client", []) if o.get("client_type") == 3]
    if web:
        values["default_web_client_id"] = web[0].get("client_id")
    return {name: value for name, value in values.items() if value}


def render(values: dict[str, str]) -> str:
    lines = ['<?xml version="1.0" encoding="utf-8"?>', "<resources>"]
    for name, value in sorted(values.items()):
        lines.append(f'    <string name="{name}" translatable="false">{escape(str(value))}</string>')
    lines.append("</resources>")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    package, source, out_dir = argv
    values = resources(json.loads(Path(source).read_text(encoding="utf-8")), package)
    target = Path(out_dir) / "values" / "google-services.xml"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(render(values), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
