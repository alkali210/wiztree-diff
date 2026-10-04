export const fileCategories: Record<string, { name: string; color: string }> = {
  code: { name: "程序/代码", color: "#78aafa" },
  images: { name: "图片", color: "#b795ed" },
  video: { name: "视频", color: "#ef87a8" },
  documents: { name: "文档", color: "#91d38d" },
  archives: { name: "压缩包", color: "#f4d978" },
  text: { name: "文本", color: "#62d0c0" },
  audio: { name: "音频", color: "#efa57d" },
  other: { name: "其他", color: "#a7afb9" },
};
export const categoryInfo = (category: string) =>
  fileCategories[category] ?? fileCategories.other;
