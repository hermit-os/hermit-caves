# Uhyve - A lightweight hypervisor for the Unikernel HermitCore

The lightweight specialized hypervisor *uhyve* is designed to improve the performance and the scalability of HermitCore applications.
In contrast to common kernels within QEMU, the boot mechanism starts directly in 64 bit mode and does not rely on inter-processor interrupts to boot additional cores.
The hypervisor is accelerated by [KVM](https://www.linux-kvm.org/) and requires Linux as host system.

To build *uhyve* you need following tools:

* x86-based Linux systems
* Recent host compiler such as GCC
* CMake	
* git

As a first step to build the hypervisor, clone the right version for your unikernel.
The `master` branch is designed for C-based version of [HermitCore](https://github.com/hermitcore/libhermit).
Please use the branch `path2rs` for the Rust-based of HermitCore (also known as [RustyHermit](https://github.com/hermitcore/libhermit-rs)). 

```sh
$ # Clone the hyvervisor for RustyHermit
$ git clone -b path2rs https://github.com/hermitcore/hermit-caves.git
```

To build the hypervisor, go to the directory with the source code and use the following commands:

```sh
$ cd hermit-caves
$ mkdir build
$ cd build
$ cmake ..
$ make
```
This will create an application *proxy* in the working directory.
Use this application to start the unikernel.

```sh
$ ./proxy /path_to_the_unikernel/hello_world
```

There are two environment variables to modify the virtual machine:
The variable `HERMIT_CPUS` specifies the number of cores the virtual machine may use.
The variable `HERMIT_MEM` defines the memory size of the virtual machine. The suffixes *M* and *G* can be used to specify a value in megabytes or gigabytes, respectively.
By default, the loader initializes a system with one core and 512 MiB RAM.
For instance, the following command starts the demo application in a virtual machine, which has 4 cores and 8GiB memory:

```bash
$ HERMIT_CPUS=4 HERMIT_MEM=8G ./proxy /path_to_the_unikernel/hello_world
```

Setting the environment variable `HERMIT_VERBOSE` to `1` makes the hypervisor print kernel log messages to the terminal.

```bash
$ HERMIT_VERBOSE=1 ./proxy /path_to_the_unikernel/hello_world
```

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
