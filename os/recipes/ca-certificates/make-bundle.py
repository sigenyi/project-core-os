#!/usr/bin/env python3
"""Build a PEM bundle of the CAs Mozilla trusts for TLS servers.

    make-bundle.py certdata.txt > ca-certificates.crt

certdata.txt holds certificate objects (CKO_CERTIFICATE) and trust objects
(CKO_NSS_TRUST). A certificate is included when its trust object, matched by issuer
and serial number, marks it CKT_NSS_TRUSTED_DELEGATOR for CKA_TRUST_SERVER_AUTH.
Only the standard library is used, so the base system can rebuild its own bundle.
"""

import base64
import sys
import textwrap


def parse(path):
    objects, obj, key, octal = [], {}, None, None
    for raw in open(path, encoding="utf-8"):
        line = raw.strip()
        if octal is not None:
            if line == "END":
                obj[key] = bytes(octal)
                octal = None
            else:
                octal.extend(int(x, 8) for x in line.split("\\")[1:])
            continue
        if not line or line.startswith("#"):
            if obj and not line:
                objects.append(obj)
                obj = {}
            continue
        parts = line.split(" ", 2)
        if parts[0] == "BEGINDATA":
            continue
        if parts[0] == "CKA_CLASS" and obj:
            objects.append(obj)
            obj = {}
        if len(parts) >= 2 and parts[1] == "MULTILINE_OCTAL":
            key, octal = parts[0], bytearray()
            continue
        if len(parts) == 3:
            value = parts[2]
            if parts[1] == "UTF8":
                value = value.strip('"')
            obj[parts[0]] = value
    if obj:
        objects.append(obj)
    return objects


def main():
    objects = parse(sys.argv[1])
    trusted = set()
    for o in objects:
        if o.get("CKA_CLASS") == "CKO_NSS_TRUST" and o.get("CKA_TRUST_SERVER_AUTH") == "CKT_NSS_TRUSTED_DELEGATOR":
            trusted.add((o.get("CKA_ISSUER"), o.get("CKA_SERIAL_NUMBER")))
    count = 0
    for o in objects:
        if o.get("CKA_CLASS") != "CKO_CERTIFICATE" or "CKA_VALUE" not in o:
            continue
        if (o.get("CKA_ISSUER"), o.get("CKA_SERIAL_NUMBER")) not in trusted:
            continue
        b64 = base64.b64encode(o["CKA_VALUE"]).decode()
        print(f"# {o.get('CKA_LABEL', 'unnamed')}")
        print("-----BEGIN CERTIFICATE-----")
        print("\n".join(textwrap.wrap(b64, 64)))
        print("-----END CERTIFICATE-----")
        count += 1
    if count < 50:
        sys.exit(f"only {count} trusted certificates found; certdata.txt format changed?")
    print(f"{count} certificates", file=sys.stderr)


if __name__ == "__main__":
    main()
