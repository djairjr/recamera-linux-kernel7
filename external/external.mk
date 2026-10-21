# Package recipes of this br2-external tree.
# None yet: the reCamera target is assembled from Buildroot and vendor
# components selected by the defconfig.
include $(sort $(wildcard $(BR2_EXTERNAL_RECAMERA_PATH)/package/*/*.mk))
