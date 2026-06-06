@echo off
pushd "%~dp0"
glslc pbr.vert -o pbr.vert.spv
glslc pbr.frag -o pbr.frag.spv
glslc brdf_lut.vert -o brdf_lut.vert.spv
glslc brdf_lut.frag -o brdf_lut.frag.spv
glslc skybox.vert -o skybox.vert.spv
glslc skybox.frag -o skybox.frag.spv
popd
