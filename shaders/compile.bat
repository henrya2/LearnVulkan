@echo off
pushd "%~dp0"
glslc pbr.vert -o pbr.vert.spv
glslc pbr.frag -o pbr.frag.spv
glslc brdf_lut.vert -o brdf_lut.vert.spv
glslc brdf_lut.frag -o brdf_lut.frag.spv
glslc skybox.vert -o skybox.vert.spv
glslc skybox.frag -o skybox.frag.spv

pushd postprocess
glslc fullscreen.vert -o fullscreen.vert.spv
glslc bright.frag -o bright.frag.spv
glslc blur.frag -o blur.frag.spv
glslc composite.frag -o composite.frag.spv
popd

popd
