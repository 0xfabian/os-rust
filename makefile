# Makefile

KERNEL := target/x86_64-unknown-none/release/kernel
ESP_DIR := esp
ESP_KERNEL := $(ESP_DIR)/boot/kernel
LIMINE_CONF := $(ESP_DIR)/boot/limine/limine.conf

IMAGE := image.hdd
IMAGE_SIZE := 256M

OVMF_CODE := ovmf/OVMF_CODE.fd
OVMF_VARS := ovmf/OVMF_VARS.fd

QEMU := qemu-system-x86_64
QEMU_FLAGS := -display sdl \
              -enable-kvm \
              -cpu host \
			  -smp $(shell nproc) \
              -m 1G \
              -drive if=pflash,format=raw,readonly=on,file=$(OVMF_CODE) \
              -drive if=pflash,format=raw,file=$(OVMF_VARS) \
              -drive file=$(IMAGE),format=raw

.PHONY: all install clean run cargo-build

all: $(IMAGE)

install: $(ESP_KERNEL)
	@sudo cp -u $(ESP_KERNEL) /boot/custom/kernel

cargo-build:
	@cargo build --release

$(ESP_KERNEL): cargo-build
	@cp -u $(KERNEL) $(ESP_KERNEL)

$(IMAGE): $(ESP_KERNEL) $(LIMINE_CONF)
	@if [ ! -f $(IMAGE) ]; then \
		echo "Creating boot image..."; \
		truncate -s $(IMAGE_SIZE) $(IMAGE); \
		sgdisk $(IMAGE) -n 1:2048 -t 1:ef00; \
		mformat -i $(IMAGE)@@1M; \
		mcopy -i $(IMAGE)@@1M -s $(ESP_DIR)/* ::; \
	else \
		echo "Updating boot image..."; \
		mcopy -i $(IMAGE)@@1M -o $(ESP_KERNEL) ::/boot; \
		mcopy -i $(IMAGE)@@1M -o $(LIMINE_CONF) ::/boot/limine; \
	fi

clean:
	@cargo clean
	rm -f $(IMAGE)
	rm -f $(ESP_KERNEL)

run: $(IMAGE)
	@$(QEMU) $(QEMU_FLAGS)
