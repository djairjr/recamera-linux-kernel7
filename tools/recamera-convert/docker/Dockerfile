# The environment of recamera-convert: Sophgo's TPU-MLIR (ONNX -> cvimodel for
# the cv181x NPU) and Ultralytics (to export a .pt to ONNX), on the
# tpuc_dev image Sophgo publishes for TPU-MLIR (Ubuntu 22.04, Python 3.10,
# torch 2.1 CPU, onnx, onnxsim already in).
FROM sophgo/tpuc_dev:v3.4

RUN pip3 install --no-cache-dir tpu_mlir==1.30.2 ultralytics==8.4.183 \
 && python3 -c "import tpu_mlir, ultralytics; print('ok')"

# Ultralytics writes its settings under $HOME; the tool runs as the caller's uid
ENV HOME=/tmp YOLO_CONFIG_DIR=/tmp/Ultralytics
