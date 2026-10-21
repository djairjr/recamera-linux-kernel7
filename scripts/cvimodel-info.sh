#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# cvimodel-info.sh -- what a .cvimodel expects and returns: chip, cvimodel
# version, input tensors (shape, dtype, pixel format, mean/scale) and output
# tensors (shape, dtype, qscale). Before adopting a model: the VPSS channel and
# the decoder must match these (F7 step 9).
#
#   scripts/cvimodel-info.sh <file.cvimodel>
#
# Runs in the build container, with the SDK's flatc and cvimodel.fbs as built
# by the sophgo-cviruntime package (make sophgo-cviruntime first).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
P=$(cd "$HERE/.." && pwd)
M=$(cd "$(dirname "${1:?usage: cvimodel-info.sh <file.cvimodel>}")" && pwd)/$(basename "$1")
case "$M" in
	"$P"/*) IN=/work/${M#"$P"/} ;;
	*) echo "the model must be inside $P (it is read in the container)" >&2; exit 1 ;;
esac

"$P/container/run.sh" python3 -I - "$IN" <<'EOF'
import glob, json, os, struct, subprocess, sys, tempfile

b = glob.glob("/work/output/sg2002_recamera_emmc/build/sophgo-cviruntime-*")[0]
flatc, fbs = b + "/host-fb/bin/flatc", b + "/deps/cvibuilder/proto/cvimodel.fbs"
d = open(sys.argv[1], "rb").read()
if d[:8] != b"CviModel":
    sys.exit("not a cvimodel")
n = struct.unpack_from("<I", d, 8)[0]
chip = d[30:46].split(b"\0")[0].decode()
with tempfile.TemporaryDirectory() as t:
    open(t + "/m.bin", "wb").write(d[48:48 + n])
    subprocess.run([flatc, "--json", "--raw-binary", "--strict-json", "-o", t, fbs, "--", t + "/m.bin"], check=True)
    j = json.load(open(t + "/m.json"))
v = j["version"]
print(f"{os.path.basename(sys.argv[1])}: {j['name']}, chip {chip}, cvimodel {v['major_']}.{v['minor_']}, built {j.get('build_time')}")
for p in j["programs"]:
    tm = {x["name"]: x for x in p["tensor_map"]}
    for kind in ("input_tensors", "output_tensors"):
        for name in p[kind]:
            x = tm[name]
            extra = ""
            if kind == "input_tensors":
                extra = f" pixel {x.get('pixel_format')} mean {x.get('mean')} scale {x.get('scale')}"
            print(f"  {kind[:-8]:6} {name} {x.get('dtype')} {x['shape']['dim']} qscale {x.get('quant', {}).get('qscale')}{extra}")
EOF
