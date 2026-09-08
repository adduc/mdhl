# Project Alpha

A setext heading, with **bold** and *emphasis*.
=============================================

## Sub Section

### Deep Section

#### Level Four

##### Level Five

###### Level Six

Plain paragraph with `inline code`, ~~strikethrough~~, an [inline
link](https://example.com), an <https://autolink.test> autolink, an
<mailto:me@example.com> email, and a [ref link][ref].

> A blockquote line.
>
> > Nested quote with **emphasis**.
>
> ```rust
> fn answer() -> i32 {
>     42
> }
> ```
>
> - list in quote
>   - nested

1. First
2. Second
   - sub item
   - another
3. Third with [link](https://x.test)

- [x] done task
- [ ] open task

| Name  | Role   | Score |
|:------|:------:|------:|
| Ada   | lead   | 100   |
| Grace | mentor | 95    |

    indented code line

```python
def main():
    # fenced with lang
    print("hello")
```

---

Inline math $a^2 + b^2 = c^2$ ends the paragraph.

<sup>[1]</sup> is a footnote reference.[^1]

[^1]: The footnote definition.

<div class="html">
  HTML block with <b>bold</b>.
</div>

[ref]: https://ref.example.org/page
