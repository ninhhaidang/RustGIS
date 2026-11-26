"""Removes what ties the generated projects to the machine that wrote them: the user QGIS records
as their author (`saveUser`, `saveUserFull`, the metadata's `<author>`) and the absolute paths a
few settings keep (a relation's or an atlas's layer source), made relative to the project. Run by
`generate.py` after it writes; also runs on its own: `python3 scrub.py OUT`.
"""
import os
import re
import sys
import zipfile

USER = re.compile(r'(saveUser|saveUserFull)="[^"]*"')
AUTHOR = re.compile(r"<author>[^<]*</author>")


def scrub_text(text, out):
    text = USER.sub(r'\1=""', text)
    text = AUTHOR.sub("<author>Unknown</author>", text)
    return text.replace(os.path.join(out, "data") + "/", "../data/")


def scrub(out):
    out = os.path.abspath(out)
    qgs_dir = os.path.join(out, "qgs")
    for name in sorted(os.listdir(qgs_dir)):
        path = os.path.join(qgs_dir, name)
        if name.endswith(".qgs"):
            with open(path, encoding="utf-8") as fh:
                text = fh.read()
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(scrub_text(text, out))
        elif name.endswith(".qgz"):
            with zipfile.ZipFile(path) as z:
                entries = [(i, z.read(i.filename)) for i in z.infolist()]
            with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
                for info, data in entries:
                    if info.filename.endswith(".qgs"):
                        data = scrub_text(data.decode("utf-8"), out).encode("utf-8")
                    z.writestr(info, data, zipfile.ZIP_DEFLATED)


if __name__ == "__main__":
    scrub(sys.argv[1])
