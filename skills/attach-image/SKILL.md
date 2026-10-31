---
name: attach-image
description: Use this to see an image.
---

Load every image you need in one call:

```python
await attach_image(*paths)
```

When not to use: to work on the pixels (size, crop, compare), open the image with PIL instead.
