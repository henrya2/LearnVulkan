@echo off
pushd "%~dp0"
glslc pbr.vert -o pbr.vert.spv
glslc pbr.frag -o pbr.frag.spv
popd
