import * as api from "./api";
import type { CardDraft } from "./api";

export async function readClipboardImage(): Promise<{ mime: string; base64: string } | null> {
  if (!navigator.clipboard?.read) throw new Error("当前环境不支持读取剪贴板图片");
  const items = await navigator.clipboard.read();
  for (const item of items) {
    const mime = ["image/png", "image/jpeg"].find((type) => item.types.includes(type));
    if (!mime) continue;
    const blob = await item.getType(mime);
    if (blob.size > 10 * 1024 * 1024) throw new Error("图片超过 10 MB，请裁剪后重试");
    const base64 = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () => reject(new Error("图片读取失败"));
      reader.onload = () => resolve(String(reader.result).split(",")[1] ?? "");
      reader.readAsDataURL(blob);
    });
    return { mime, base64 };
  }
  return null;
}

export const extractCards = (mime: string, base64: string) => api.extractFlashcards(mime, base64);
export const saveFlashcards = (subjectId: string, topicId: string | null, cards: CardDraft[], title: string) =>
  api.saveFlashcards(subjectId, topicId, cards, title);
