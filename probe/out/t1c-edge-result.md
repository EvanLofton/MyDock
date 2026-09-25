# T1c 锐利边缘法：毛玻璃模糊量测

基线（无玻璃窗）：过渡宽度 **1.0 px**，对比度 255

> 过渡宽度 ≈ 1-2px 表示边缘没被模糊；宽度明显变大表示真的模糊了。

| 路线 | 样式 | 焦点 | 过渡宽度(px) | 对比度 | 判定 |
|---|---|---|---|---|---|
| ctl-none | plain | focused | 1.0 | 255 | **无模糊** |
| ctl-none | plain | unfocused | 1.0 | 255 | **无模糊** |
| ctl-none | plain | noactivate | 1.0 | 255 | **无模糊** |
| ctl-none | layered | focused | 1.0 | 255 | **无模糊** |
| ctl-none | layered | unfocused | 1.0 | 255 | **无模糊** |
| ctl-none | layered | noactivate | 1.0 | 255 | **无模糊** |
| ctl-none | noredir | focused | 1.0 | 255 | **无模糊** |
| ctl-none | noredir | unfocused | 1.0 | 255 | **无模糊** |
| ctl-none | noredir | noactivate | 1.0 | 255 | **无模糊** |
| ctl-opaque | plain | focused | inf | 0 | **边缘完全消失(强模糊或纯色)** |
| ctl-opaque | plain | unfocused | inf | 0 | **边缘完全消失(强模糊或纯色)** |
| ctl-opaque | plain | noactivate | inf | 0 | **边缘完全消失(强模糊或纯色)** |
| ctl-opaque | layered | focused | 1.0 | 30 | **边缘锐利但对比度低** |
| ctl-opaque | layered | unfocused | 1.0 | 30 | **边缘锐利但对比度低** |
| ctl-opaque | layered | noactivate | 1.0 | 30 | **边缘锐利但对比度低** |
| ctl-opaque | noredir | focused | 1.0 | 255 | **无模糊** |
| ctl-opaque | noredir | unfocused | 1.0 | 255 | **无模糊** |
| ctl-opaque | noredir | noactivate | 1.0 | 255 | **无模糊** |
| A-acrylic | plain | focused | 76.0 | 92 | **强模糊** |
| A-acrylic | plain | unfocused | 76.0 | 92 | **强模糊** |
| A-acrylic | plain | noactivate | 76.0 | 92 | **强模糊** |
| A-acrylic | layered | focused | 66.0 | 111 | **强模糊** |
| A-acrylic | layered | unfocused | 66.0 | 111 | **强模糊** |
| A-acrylic | layered | noactivate | 66.0 | 111 | **强模糊** |
| A-acrylic | noredir | focused | 76.0 | 92 | **强模糊** |
| A-acrylic | noredir | unfocused | 76.0 | 92 | **强模糊** |
| A-acrylic | noredir | noactivate | 76.0 | 92 | **强模糊** |
| A-mica | plain | focused | inf | 1 | **边缘完全消失(强模糊或纯色)** |
| A-mica | plain | unfocused | inf | 1 | **边缘完全消失(强模糊或纯色)** |
| A-mica | plain | noactivate | inf | 1 | **边缘完全消失(强模糊或纯色)** |
| A-mica | layered | focused | 1.0 | 31 | **边缘锐利但对比度低** |
| A-mica | layered | unfocused | 1.0 | 31 | **边缘锐利但对比度低** |
| A-mica | layered | noactivate | 1.0 | 31 | **边缘锐利但对比度低** |
| A-mica | noredir | focused | inf | 1 | **边缘完全消失(强模糊或纯色)** |
| A-mica | noredir | unfocused | inf | 1 | **边缘完全消失(强模糊或纯色)** |
| A-mica | noredir | noactivate | inf | 1 | **边缘完全消失(强模糊或纯色)** |
| B-acrylic | plain | focused | 82.0 | 62 | **强模糊** |
| B-acrylic | plain | unfocused | 82.0 | 62 | **强模糊** |
| B-acrylic | plain | noactivate | 82.0 | 62 | **强模糊** |
| B-acrylic | layered | focused | 62.0 | 84 | **强模糊** |
| B-acrylic | layered | unfocused | 62.0 | 84 | **强模糊** |
| B-acrylic | layered | noactivate | 62.0 | 84 | **强模糊** |
| B-acrylic | noredir | focused | 82.0 | 62 | **强模糊** |
| B-acrylic | noredir | unfocused | 82.0 | 62 | **强模糊** |
| B-acrylic | noredir | noactivate | 82.0 | 62 | **强模糊** |
