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
	@test -f "$(FADERPUNK_DIR)/Cargo.toml" || { \
		echo "error: FADERPUNK_DIR ($(FADERPUNK_DIR)) is not a Faderpunk checkout" >&2; \
		echo "       clone https://github.com/ATOVproject/faderpunk next to this repo, or pass FADERPUNK_DIR=/path/to/faderpunk" >&2; \
		exit 1; \
	}
	cargo run --manifest-path "$(FADERPUNK_DIR)/Cargo.toml" -p fpapp -- \
		build-community \
		--repo "$(CURDIR)" \
		--output "$(CURDIR)/$(FPAPP_OUTPUT)" \
		$(FPAPP_FIRMWARE_ARG)
