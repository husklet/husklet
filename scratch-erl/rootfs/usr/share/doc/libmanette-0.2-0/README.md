# libmanette

The simple GObject game controller library.

libmanette offers painless access to game controllers, from any programming
language and with little dependencies. 

It supports the de-facto standard gamepad, as defined by the
[W3C standard gamepad specification](https://www.w3.org/TR/gamepad/) or as
implemented by the
[SDL GameController](https://wiki.libsdl.org/CategoryGameController).
Convertion of raw gamepad events into usable ones is handled transparently using
an embedded library of mappings in the popular SDL mapping string format.

The API is inspired by the device and event handling of GDK, so anybody used to
[GTK](https://gtk.org/) should feel right at home.

## Building and Installing the Library

Meson is the buildsystem of libmanette, to build and install it run:

	# meson build
	# cd build
	# ninja
	# ninja install

## Testing the Library

libmanette comes with the manette-test tool, which will display events
from game controllers. You can run it from the build directory with:

	# demos/manette-test/manette-test
