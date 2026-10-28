#!/bin/sh
# The name release files carry: the tag (v0.2.0) for a release, otherwise dev-<commit>.
case "${GITHUB_REF:-}" in
    refs/tags/*) echo "${GITHUB_REF#refs/tags/}" ;;
    *) echo "dev-$(git rev-parse --short HEAD)" ;;
esac
