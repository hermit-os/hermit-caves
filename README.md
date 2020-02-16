# Uhyve - A lightweight hypervisor for the Unikernel HermitCore

*uhyve* is designed to improve the performance and the scalability of HermitCore / RustyHermit applications.
In contrast to QEMU, uhyve directly boots in 64 bit mode and does not rely on inter-processor interrupts to boot additional cores.
The hypervisor is uses [KVM](https://www.linux-kvm.org/) and requires Linux as host system.

## Dependencies

To build *uhyve* you need following tools:

* x86-based Linux systems
* Recent host compiler such as GCC
* CMake
* git

## Build

*Please make sure that you use the correct branch of this repository.*

The `master` branch is designed for [HermitCore](https://github.com/hermitcore/libhermit), whereas the branch `path2rs` is required for [RustyHermit](https://github.com/hermitcore/libhermit-rs).

As a first step please clone this repository. This will clone the uhyve version used for RustyHermit

```sh
git clone -b path2rs https://github.com/hermitcore/hermit-caves.git
```

The build process itself is identical for both uhyve versions:

```sh
cd hermit-caves
mkdir build
cd build
cmake ..
make
```

This will create an application *uhyve* in the working directory.
Use this application to start the RustyHermit applications.

## Usage

uhyve is configured by environment variables.
The variable `HERMIT_CPUS` specifies the number of cores the RustyHermit application can use and `HERMIT_MEM` defines the maximum amount of memory available to the application. The suffixes *M* and *G* can be used to specify a value in megabytes or gigabytes, respectively.
By default, uhyve will use one core and 512 MiB RAM.
For instance, the following command starts the demo application with 4 cores and 8 GiB memory:

```sh
HERMIT_CPUS=4 HERMIT_MEM=8G ./uhyve ../../hello_world/target/x86_64-unknown-hermit/debug/hello_world
```

Setting the environment variable `HERMIT_VERBOSE` to `1` will have uhyve print kernel log messages to the terminal.

```sh
HERMIT_VERBOSE=1 ./uhyve ../../hello_world/target/x86_64-unknown-hermit/debug/hello_world
```

## License

Licensed under either of

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
