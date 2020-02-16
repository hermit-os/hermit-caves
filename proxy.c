/*
 * Copyright (c) 2015, Stefan Lankes, RWTH Aachen University
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions are met:
 *    * Redistributions of source code must retain the above copyright
 *      notice, this list of conditions and the following disclaimer.
 *    * Redistributions in binary form must reproduce the above copyright
 *      notice, this list of conditions and the following disclaimer in the
 *      documentation and/or other materials provided with the distribution.
 *    * Neither the name of the University nor the names of its contributors
 *      may be used to endorse or promote products derived from this
 *      software without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
 * DISCLAIMED. IN NO EVENT SHALL THE REGENTS OR CONTRIBUTORS BE LIABLE FOR ANY
 * DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
 * (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
 * LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
 * ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
 * SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/tcp.h>
#include <net/if.h>
#include <netinet/in.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdbool.h>
#include <string.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include "proxy.h"

#define MAX_PATH	255
#define MAX_ARGS	1024
#define INADDR(a, b, c, d) (struct in_addr) { .s_addr = ((((((d) << 8) | (c)) << 8) | (b)) << 8) | (a) }

#define HERMIT_PORT	0x494E
#define HERMIT_IP(isle)	INADDR(192, 168, 28, isle + 2)
#define HERMIT_MAGIC	0x7E317

#define EVENT_SIZE	(sizeof (struct inotify_event))
#define BUF_LEN		(1024 * (EVENT_SIZE + 16))

#if 0
#define PROXY_DEBUG(fmt, ...) fprintf(stderr, fmt, ##__VA_ARGS__);
#else
#define PROXY_DEBUG(fmt, ...) {}
#endif

bool verbose = false;

static int sobufsize = 131072;
static unsigned int port = HERMIT_PORT;

extern char **environ;

static void exit_handler(int sig)
{
	exit(0);
}

static int env_init(char *path)
{
	char* str;
	struct sigaction sINT, sTERM;

	// define action for SIGINT
	sINT.sa_handler = exit_handler;
	sINT.sa_flags = 0;
	if (sigaction(SIGINT, &sINT, NULL) < 0)
	{
		perror("sigaction");
		exit(1);
	}

	// define action for SIGTERM
	sTERM.sa_handler = exit_handler;
	sTERM.sa_flags = 0;
	if (sigaction(SIGTERM, &sTERM, NULL) < 0)
	{
		perror("sigaction");
		exit(1);
	}

	str = getenv("HERMIT_PORT");
	if (str)
	{
		port = atoi(str);
		if ((port == 0) || (port >= UINT16_MAX))
			port = HERMIT_PORT;
	}

	return uhyve_init(path);
}

/*
 * in principle, HermitCore forwards basic system calls to
 * this proxy, which mapped these call to Linux system calls.
 */
int handle_syscalls(int s)
{
	int sysnr;
	ssize_t sret;
	size_t j;

	while(1)
	{
		j = 0;
		while(j < sizeof(sysnr)) {
			sret = read(s, ((char*)&sysnr)+j, sizeof(sysnr)-j);
			if (sret < 0)
				goto out;
			j += sret;
		}

		switch(sysnr)
		{
		case __HERMIT_exit: {
			int arg = 0;

			j = 0;
			while(j < sizeof(arg)) {
				sret = read(s, ((char*)&arg)+j, sizeof(arg)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}
			close(s);

			if (arg == -14)
				fprintf(stderr, "Did HermitCore receive an exception?\n");
			exit(arg);
			break;
		}
		case __HERMIT_write: {
			int fd;
			size_t len;
			char* buff;

			j = 0;
			while (j < sizeof(fd)) {
				sret = read(s, ((char*)&fd)+j, sizeof(fd)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			j = 0;
			while (j < sizeof(len)) {
				sret = read(s, ((char*)&len)+j, sizeof(len)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			buff = malloc(len);
			if (!buff) {
				fprintf(stderr,"Proxy: not enough memory");
				return 1;
			}

			j=0;
			while(j < len)
			{
				sret = read(s, buff+j, len-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			if (fd > 2) {
				sret = write(fd, buff, len);
				j = 0;
				while(j < sizeof(sret))
				{
					int l = write(s, ((char*)&sret)+j, sizeof(sret)-j);
					if (l < 0)
						goto out;
					j += l;
				}
			} else {
				j = 0;
				while(j < len)
				{
					sret = write(fd, buff+j, len-j);
					if (sret < 0)
						goto out;
					j += sret;
				}
			}

			free(buff);
			break;
		}
		case __HERMIT_open: {
			size_t len;
			char* fname;
			int flags, mode, ret;

			j = 0;
			while (j < sizeof(len))
			{
				sret = read(s, ((char*)&len)+j, sizeof(len)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			fname = malloc(len);
			if (!fname)
				goto out;

			j = 0;
			while (j < len)
			{
				sret = read(s, fname+j, len-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			j = 0;
			while (j < sizeof(flags))
			{
				sret = read(s, ((char*)&flags)+j, sizeof(flags)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			j = 0;
			while (j < sizeof(mode))
			{
				sret = read(s, ((char*)&mode)+j, sizeof(mode)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			//printf("flags 0x%x, mode 0x%x\n", flags, mode);

			ret = open(fname, flags, mode);
			j = 0;
			while(j < sizeof(ret))
			{
				sret = write(s, ((char*)&ret)+j, sizeof(ret)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			free(fname);
			break;
		}
		case __HERMIT_close: {
			int fd, ret;

			j = 0;
			while(j < sizeof(fd))
			{
				sret = read(s, ((char*)&fd)+j, sizeof(fd)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			if (fd > 2)
				ret = close(fd);
			else
				ret = 0;

			j = 0;
			while (j < sizeof(ret))
			{
				sret = write(s, ((char*)&ret)+j, sizeof(ret)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}
			break;
		}
		case __HERMIT_read: {
			int fd, flag;
			size_t len;
			ssize_t sj;
			char* buff;

			j = 0;
			while(j < sizeof(fd))
			{
				sret = read(s, ((char*)&fd)+j, sizeof(fd)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			j = 0;
			while(j < sizeof(len))
			{
				sret = read(s, ((char*)&len)+j, sizeof(len)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			buff = malloc(len);
			if (!buff)
				goto out;

			sj = read(fd, buff, len);

			flag = 0;
			setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (char *) &flag, sizeof(int));

			j = 0;
			while (j < sizeof(sj))
			{
				sret = write(s, ((char*)&sj)+j, sizeof(sj)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			if (sj > 0)
			{
				size_t i = 0;

				while (i < sj)
				{
					sret = write(s, buff+i, sj-i);
					if (sret < 0)
						goto out;

					i += sret;
				}
			}

			flag = 1;
			setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (char *) &flag, sizeof(int));

			free(buff);
			break;
		}
		case __HERMIT_lseek: {
			int fd, whence;
			off_t offset;

			j = 0;
			while (j < sizeof(fd))
			{
				sret = read(s, ((char*)&fd)+j, sizeof(fd)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			j = 0;
			while (j < sizeof(offset))
			{
				sret = read(s, ((char*)&offset)+j, sizeof(offset)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			j = 0;
			while (j < sizeof(whence))
			{
				sret = read(s, ((char*)&whence)+j, sizeof(whence)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}

			offset = lseek(fd, offset, whence);

			j = 0;
			while (j < sizeof(offset))
			{
				sret = write(s, ((char*)&offset)+j, sizeof(offset)-j);
				if (sret < 0)
					goto out;
				j += sret;
			}
			break;
		}
		default:
			fprintf(stderr, "Proxy: invalid syscall number %d, errno %d, ret %zd\n", sysnr, errno, sret);
			close(s);
			exit(1);
			break;
		}
	}

out:
	perror("Proxy -- communication error");

	return 1;
}

int socket_loop(int argc, char **argv)
{
	int i, j, ret, s;
	int32_t magic = HERMIT_MAGIC;
	struct sockaddr_in serv_name;

#if 0
	// check if mmnif interface is available
	if (!qemu) {
		struct ifreq ethreq;

		memset(&ethreq, 0, sizeof(ethreq));
		strncpy(ethreq.ifr_name, "mmnif", IFNAMSIZ);

		while(1) {
			/* this socket doesn't really matter, we just need a descriptor
			 * to perform the ioctl on */
			s = socket(PF_INET, SOCK_STREAM, IPPROTO_TCP);
			ioctl(s, SIOCGIFFLAGS, &ethreq);
			close(s);

			if (ethreq.ifr_flags & (IFF_UP|IFF_RUNNING))
				break;
		}
		sched_yield();
	}
#endif

	/* create a socket */
	s = socket(PF_INET, SOCK_STREAM, 0);
	if (s < 0)
	{
		perror("Proxy: socket creation error");
		exit(1);
	}

	setsockopt(s, SOL_SOCKET, SO_RCVBUF, (char *) &sobufsize, sizeof(sobufsize));
	setsockopt(s, SOL_SOCKET, SO_SNDBUF, (char *) &sobufsize, sizeof(sobufsize));
	i = 1;
	setsockopt(s, IPPROTO_TCP, TCP_NODELAY, (char *) &i, sizeof(i));
	i = 0;
	setsockopt(s, SOL_SOCKET, SO_KEEPALIVE, (char *) &i, sizeof(i));

	/* server address  */
	memset((char *) &serv_name, 0x00, sizeof(serv_name));
	serv_name.sin_family = AF_INET;
	serv_name.sin_addr = HERMIT_IP(0);
	serv_name.sin_port = htons(port);

	i = 0;
retry:
	ret = connect(s, (struct sockaddr*)&serv_name, sizeof(serv_name));
	if (ret < 0)
	{
		i++;
		if (i <= 10) {
			usleep(10000);
			goto retry;
		}
		perror("Proxy -- connection error");
		close(s);
		exit(1);
	}

	ret = write(s, &magic, sizeof(magic));
	if (ret < 0)
		goto out;

	// forward program arguments to HermitCore
	// argv[0] is path of this proxy so we strip it

	argv++;
	argc--;

	ret = write(s, &argc, sizeof(argc));
	if (ret < 0)
		goto out;

	for(i=0; i<argc; i++)
	{
		int len = strlen(argv[i])+1;

		j = 0;
		while (j < sizeof(len))
		{
			ret = write(s, ((char*)&len)+j, sizeof(len)-j);
			if (ret < 0)
				goto out;
			j += ret;
		}

		j = 0;
		while (j < len)
		{
			ret = write(s, argv[i]+j, len-j);
			if (ret < 0)
				goto out;
			j += ret;
		}
	}

	// send environment
	i = 0;
	while(environ[i])
		i++;

	ret = write(s, &i, sizeof(i));
	if (ret < 0)
		goto out;

	for(i=0; environ[i] ;i++)
	{
		int len = strlen(environ[i])+1;

		j = 0;
		while (j < sizeof(len))
		{
			ret = write(s, ((char*)&len)+j, sizeof(len)-j);
			if (ret < 0)
				goto out;
			j += ret;
		}

		j = 0;
		while (j < len)
		{
			ret = write(s, environ[i]+j, len-j);
			if (ret < 0)
				goto out;
			j += ret;
		}
	}

	ret = handle_syscalls(s);

	close(s);

	return ret;

out:
	perror("Proxy -- communication error");
	close(s);
	return 1;
}

int main(int argc, char **argv)
{
	int ret;

	char* v = getenv("HERMIT_VERBOSE");
	if (v && (strcmp(v, "0") != 0)) {
		//fprintf(stderr, "Start proxy in verbose mode\n");
		verbose = true;
	}

	ret = env_init(argv[1]);
	if (ret)
		return ret;


	return uhyve_loop(argc, argv);
}
