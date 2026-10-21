# recamera-convert

Convert a YOLOv8/YOLO11 model — **detect, pose, segment or classify**, a `.pt` or an
`.onnx` — to a `.cvimodel` for the reCamera's cv181x NPU, with the `.json` the camera
reads. Upload both on the camera's YOLO page and press Use.

```sh
recamera-convert dog-pose.pt --images calibration/ -o out/
# out/dog-pose.cvimodel  out/dog-pose.json
```

## Build and requirements

- Docker. The first run builds the image `recamera-convert:1` from `docker/Dockerfile`:
  Sophgo's `tpuc_dev:v3.4` (the environment of their converter, TPU-MLIR) with
  `tpu_mlir` 1.30.2 and Ultralytics 8.4.183. A few GB, once.
- Rust to build the tool: `cargo build --release` here (`target/release/recamera-convert`).
  The project's build container works too:
  `container/run.sh sh -c 'cd /work/tools/recamera-convert && cargo build --release'`.

The conversion cannot run on the camera (TPU-MLIR is an MLIR/LLVM compiler for x86 with
several GB of dependencies, and calibration runs the whole network in floating point), and
reimplementing it in Rust is out of reach: the program drives Sophgo's tools in the
container and does everything around them.

## What it does

1. **Export** (a `.pt`): Ultralytics to ONNX, opset 17, fixed input size (`--imgsz`, 640).
   An official name that is not a file (`yolo11n-pose.pt`) is downloaded by Ultralytics.
2. **Inspect** the ONNX: input, outputs, and the metadata Ultralytics writes (class names,
   task, `kpt_shape`).
3. **Check** that the outputs have a layout the camera decodes — the same
   `recamera_detect::decode::layout` recamerad uses — and that names, task and keypoints
   agree with it. A model the camera could not read stops here.
4. **Convert** with TPU-MLIR: `model_transform` (with the `/255` of the input folded into the
   model), `run_calibration` on your pictures, `model_deploy` for `cv181x` with
   `--fuse_preprocess --customization_format RGB_PACKED` (the camera feeds uint8 RGB from the
   video pipeline, no CPU work), INT8 with a **BF16 decoding tail**.
5. **Write** `<name>.cvimodel` and `<name>.json` (`classes`, `task`, `kpt_shape`,
   `description`, and whatever `--extra FILE.json` adds, e.g. a pose skeleton).

### Why the BF16 tail

YOLOv8/11 export ends in a decoding tail (DFL, sigmoid, box and keypoint arithmetic, a
final concat of boxes, scores and keypoints). In INT8 the concat shares one scale between
coordinates (0..640) and scores (0..1): every score comes out 0 and the model finds
nothing. The factory pose and segment models keep that tail in BF16 (their coordinates
move in steps of 2 px, BF16's precision there). The tool walks the ONNX graph back from the
outputs to the head's last convolutions and lists those ops in the TPU-MLIR quantize table
as BF16.

Two pitfalls met on the way:
- TPU-MLIR names the ops of its MLIR `<ONNX tensor>_<op type>` and **silently ignores**
  quantize-table names it does not know; the table is built from the names in the MLIR.
- TPU-MLIR's own check (`--test_reference`, simulated INT8 against the float ONNX) passed
  with the scores zeroed: the similarity is dominated by the coordinates. Check a converted
  model on the camera (`recamera-yolo --stats` prints the range of each output channel).

## Calibration pictures

INT8 needs the range of every layer, measured on real inputs: give 100 or more pictures
like the ones the camera will see (same place, light, subjects). Camera snapshots
(`http://<camera>/snapshot.jpg`) are ideal. Any `.jpg`/`.png`/`.bmp` under the folder is
used.

## Pose models

The camera deduces the keypoint count from the outputs (17 for COCO people, 24 for the
Ultralytics dog-pose dataset, any other). To draw lines, add the skeleton (pairs of keypoint
indices from 0) and, optionally, names:

```json
{ "skeleton": [[0, 1], [1, 2]], "kpt_names": ["nose", "left_eye", "right_eye"] }
```

```sh
recamera-convert dog-pose.pt --images calibration/ --extra dog-skeleton.json
```

## Measured

Calibrated on 100 COCO pictures, about 2 min 45 s each on a laptop CPU, then run on the
camera on the same test picture as the factory (Seeed) models:

| Model | Factory | Converted |
|---|---|---|
| YOLO11n pose (`yolo11n-pose.pt`) | score 0.84, 53 ms | score 0.74, 50 ms; box and keypoints within a few pixels |
| YOLO11n detect (`yolo11n.pt`) | score 0.61, 57 ms | score 0.61, 71 ms with all 80 classes decoded on the CPU; box within 12 px |

The factory detection model is the Sophgo split-head export; the converted one is the plain
Ultralytics output, which the camera decodes as well (on one class, as recamerad does, the
CPU part is small).
