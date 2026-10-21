#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The steps of recamera-convert that need Python, run inside its container
(TPU-MLIR and Ultralytics only exist as Python tools). Each prints one JSON
object on its last line of stdout for the Rust side.

  convert.py export <model.pt> <imgsz>          -> {"onnx": path}
  convert.py inspect <model.onnx>               -> {"inputs": .., "outputs": .., "metadata": ..}
  convert.py convert <model.onnx> <name> <images dir> <count> <workdir>
                                                 -> {"cvimodel": path, "bf16_ops": n}
"""
import glob
import json
import os
import subprocess
import sys


def export(pt, imgsz):
    from ultralytics import YOLO
    path = YOLO(pt).export(format="onnx", imgsz=int(imgsz), opset=17, simplify=True, dynamic=False)
    print(json.dumps({"onnx": str(path)}))


def dims(vi):
    return [d.dim_value if d.HasField("dim_value") else -1 for d in vi.type.tensor_type.shape.dim]


def inspect(path):
    import ast
    import onnx
    m = onnx.load(path)
    init = {i.name for i in m.graph.initializer}
    meta = {p.key: p.value for p in m.metadata_props}
    # Ultralytics writes these as Python literals: {0: 'person', ...}, [17, 3]
    lit = lambda k: ast.literal_eval(meta[k]) if k in meta else None
    names = lit("names")
    if isinstance(names, dict):
        names = [names[k] for k in sorted(names)]
    print(json.dumps({
        "inputs": [{"name": i.name, "shape": dims(i)} for i in m.graph.input if i.name not in init],
        "outputs": [{"name": o.name, "shape": dims(o)} for o in m.graph.output],
        "task": meta.get("task"),
        "names": names,
        "kpt_shape": lit("kpt_shape"),
        "description": meta.get("description"),
        "version": meta.get("version"),
    }))


# The decoding tail of a YOLOv8/11 head: from the outputs back to the last
# convolutions of the head. In INT8 it would share one scale between box
# coordinates (0..640) and scores (0..1) at the final concat and lose the
# scores, so it is kept in BF16 (as the factory pose/segment models are).
# The DFL is a convolution inside the tail, not a boundary.
BOUNDARY = {"Conv", "Gemm", "MatMul"}


def tail_ops(path):
    import onnx
    m = onnx.load(path)
    producer = {}
    for n in m.graph.node:
        for o in n.output:
            producer[o] = n
    seen, stack, tail = set(), [o.name for o in m.graph.output], []
    while stack:
        t = stack.pop()
        n = producer.get(t)
        if n is None or id(n) in seen:
            continue
        seen.add(id(n))
        if n.op_type in BOUNDARY and "dfl" not in n.name.lower():
            continue
        if n.op_type not in ("Constant", "Shape"):
            tail.extend(n.output)
        stack.extend(n.input)
    return tail


def run(cmd, log):
    print("+", " ".join(cmd), file=sys.stderr, flush=True)
    with open(log, "a") as f:
        r = subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT)
    if r.returncode != 0:
        with open(log) as f:
            tail = f.read()[-3000:]
        sys.exit(f"{cmd[0]} failed (exit {r.returncode}); end of {log}:\n{tail}")


def convert(onnx_path, name, images, count, work):
    import onnx
    os.chdir(work)
    log = os.path.join(work, "tpu-mlir.log")
    m = onnx.load(onnx_path)
    init = {i.name for i in m.graph.initializer}
    inp = next(i for i in m.graph.input if i.name not in init)
    n, c, h, w = dims(inp)
    pics = sorted(p for p in glob.glob(os.path.join(images, "**", "*"), recursive=True)
                  if p.lower().endswith((".jpg", ".jpeg", ".png", ".bmp")))
    if not pics:
        sys.exit(f"no images (.jpg/.png/.bmp) in {images}")
    test = pics[0]
    # the model takes RGB 0..1 (Ultralytics); the camera feeds uint8 RGB
    # packed, letterboxed: the /255 goes into the model (fuse_preprocess)
    run(["model_transform", "--model_name", name, "--model_def", onnx_path,
         "--input_shapes", f"[[1,{c},{h},{w}]]", "--mean", "0.0,0.0,0.0",
         "--scale", "0.0039216,0.0039216,0.0039216", "--keep_aspect_ratio", "--pixel_format", "rgb",
         "--test_input", test, "--test_result", f"{name}_top_outputs.npz", "--mlir", f"{name}.mlir"], log)
    run(["run_calibration", f"{name}.mlir", "--dataset", images, "--input_num", str(count),
         "-o", f"{name}_cali_table"], log)
    # the qtable names MLIR ops, which TPU-MLIR calls "<ONNX tensor>_<op type>"
    # (unknown names are ignored without a word): take the ops of the
    # transformed model whose tensor is in the tail
    import re
    tail = set(tail_ops(onnx_path))
    with open(f"{name}.mlir") as f:
        locs = sorted(set(re.findall(r'loc\("([^"]+)"\)', f.read())))
    ops = [l for l in locs if l in tail or l.rsplit("_", 1)[0] in tail]
    if tail and not ops:
        sys.exit("none of the decoding tail's ops found in the MLIR: cannot keep it in BF16")
    with open("qtable", "w") as f:
        for op in ops:
            f.write(f"{op} BF16\n")
    run(["model_deploy", "--mlir", f"{name}.mlir", "--quantize", "INT8", "--quantize_table", "qtable",
         "--calibration_table", f"{name}_cali_table", "--processor", "cv181x",
         "--fuse_preprocess", "--customization_format", "RGB_PACKED",
         # with fuse_preprocess the test input is the picture itself
         "--test_input", test, "--test_reference", f"{name}_top_outputs.npz",
         "--tolerance", "0.85,0.45", "--model", f"{name}.cvimodel"], log)
    print(json.dumps({"cvimodel": os.path.join(work, f"{name}.cvimodel"), "bf16_ops": len(ops),
                      "test_image": os.path.basename(test)}))


if __name__ == "__main__":
    a = sys.argv[1:]
    if a[:1] == ["export"]:
        export(a[1], a[2])
    elif a[:1] == ["inspect"]:
        inspect(a[1])
    elif a[:1] == ["convert"]:
        convert(a[1], a[2], a[3], int(a[4]), a[5])
    else:
        sys.exit(__doc__)
