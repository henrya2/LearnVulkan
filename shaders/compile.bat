@echo off
pushd "%~dp0"
glslc -g pbr.vert -o pbr.vert.spv
glslc -g pbr.frag -o pbr.frag.spv
glslc -g brdf_lut.vert -o brdf_lut.vert.spv
glslc -g brdf_lut.frag -o brdf_lut.frag.spv
glslc -g skybox.vert -o skybox.vert.spv
glslc -g skybox.frag -o skybox.frag.spv

pushd postprocess
glslc -g fullscreen.vert -o fullscreen.vert.spv
glslc -g bright.frag -o bright.frag.spv
glslc -g blur.frag -o blur.frag.spv
glslc -g composite.frag -o composite.frag.spv
popd

popd
