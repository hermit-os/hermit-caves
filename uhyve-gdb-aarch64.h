/*
 * This file was adapted from the solo5/ukvm code base, initial copyright block
 * follows:
 */

/*
 * Copyright (c) 2015-2017 Contributors as noted in the AUTHORS file
 *
 * This file is part of ukvm, a unikernel monitor.
 *
 * Permission to use, copy, modify, and/or distribute this software
 * for any purpose with or without fee is hereby granted, provided
 * that the above copyright notice and this permission notice appear
 * in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
 * WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
 * AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR
 * CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS
 * OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
 * NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
 * CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

#ifndef UHYVE_GDB_AARCH64_H
#define UHYVE_GDB_AARCH64_H

#include <inttypes.h>
#include <stdint.h>

struct uhyve_gdb_regs {
	uint64_t x[31];
	uint64_t sp;
	uint64_t pc;
	uint32_t cpsr;
	/* we need to pack this structure so that the compiler does not insert any
	 * padding (for example to have the structure size being a multiple of 8
	 * bytes. Indeed, when asking the server for the values of the registers,
	 * gdb client looks at the size of the response (function of the size of
	 * this structure) to determine which registers are concerned. They are
	 * furthermore determine by a convention of order and sizes according to
	 * the following: https://github.com/bminor/binutils-gdb/blob/master/gdb/
	 * features/aarch64-core.xml
	 */
} __attribute__((__packed__));

#endif /* UHYVE_GDB_AARCH64_H */
