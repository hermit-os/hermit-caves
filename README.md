# hermit-caves

This repository contains **uhyve**, the hypervisor to start RustyHermit applications.

## Dependencies

To build *uhyve* you need following tools:

* x86-based Linux systems
* Recent host compiler such as GCC
* CMake	
* git

## Build

As a first step please clone this repository

```sh
git clone -b path2rs https://github.com/hermitcore/hermit-caves.git
```

and than execute the following commands to build uhyve.

```sh
cd hermit-caves
mkdir build
cd build
cmake ..
make
```
This will create an application *proxy* in the working directory.
Use this application to start the RustyHermit applications.

## Usage

uhyve is configured by environment variables.
The variable `HERMIT_CPUS` specifies the number of cores the RustyHermit application can use and `HERMIT_MEM` defines the maximum amount of memory available to the application. The suffixes *M* and *G* can be used to specify a value in megabytes or gigabytes, respectively.
By default, uhyve will use one core and 512 MiB RAM.
For instance, the following command starts the demo application with 4 cores and 8 GiB memory:

```sh
HERMIT_CPUS=4 HERMIT_MEM=8G ./proxy ../../hello_world/target/x86_64-unknown-hermit/debug/hello_world
```

Setting the environment variable `HERMIT_VERBOSE` to `1` will have uhyve print kernel log messages to the terminal.

```sh
HERMIT_VERBOSE=1 ./proxy ../../hello_world/target/x86_64-unknown-hermit/debug/hello_world
```
