# muse-box for macOS. Needs only the Xcode Command Line Tools
# (xcode-select --install); full Xcode works too.

# Command Line Tools ship swift-testing outside the default search path.
CLT_FRAMEWORKS := /Library/Developer/CommandLineTools/Library/Developer/Frameworks
CLT_LIBS := /Library/Developer/CommandLineTools/Library/Developer/usr/lib
TEST_FLAGS := $(if $(findstring CommandLineTools,$(shell xcode-select -p)),\
	-Xswiftc -F -Xswiftc $(CLT_FRAMEWORKS) -Xlinker -F -Xlinker $(CLT_FRAMEWORKS) \
	-Xlinker -rpath -Xlinker $(CLT_FRAMEWORKS) -Xlinker -rpath -Xlinker $(CLT_LIBS))

.PHONY: app run test zip stills icon clean

app:
	./scripts/build-app.sh

run: app
	open build/muse-box.app

test:
	swift test $(TEST_FLAGS)

zip: app
	ditto -c -k --keepParent build/muse-box.app build/muse-box-macos.zip
	@echo build/muse-box-macos.zip

# Renders the interface to docs/*.png with a staged track: no Spotify needed.
stills:
	swift build
	MUSEBOX_STILL_SCALE=1.5 .build/debug/muse-box --snapshot docs

icon:
	swift scripts/make-icon.swift build/AppIcon.iconset
	iconutil -c icns build/AppIcon.iconset -o Resources/AppIcon.icns

clean:
	rm -rf .build build
