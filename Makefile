FADERPUNK_DIR ?= ../faderpunk
FPAPP_OUTPUT ?= build/fpapps
FIRMWARE_REVISION ?= $(shell git -C "$(FADERPUNK_DIR)" rev-parse HEAD)

ifdef FIRMWARE_ABI
FPAPP_FIRMWARE_ARG = --firmware-abi "$(FIRMWARE_ABI)"
else
FPAPP_FIRMWARE_ARG = --firmware-revision "$(FIRMWARE_REVISION)"
endif

.PHONY: fpapps
fpapps:
	cargo run --manifest-path "$(FADERPUNK_DIR)/Cargo.toml" -p fpapp -- \
		build-community \
		--repo "$(CURDIR)" \
		--output "$(CURDIR)/$(FPAPP_OUTPUT)" \
		$(FPAPP_FIRMWARE_ARG)
